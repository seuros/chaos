use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::error::GuestError;
use crate::handler::ClientHandler;
use crate::protocol::CancelledNotificationParams;
use crate::protocol::ClientCapabilities;
use crate::protocol::CreateElicitationRequest;
use crate::protocol::CreateMessageRequest;
use crate::protocol::ElicitationCompleteNotificationParams;
use crate::protocol::Implementation;
use crate::protocol::InitializeRequest;
use crate::protocol::InitializeResult;
use crate::protocol::JsonRpcError;
use crate::protocol::JsonRpcMessage;
use crate::protocol::JsonRpcRequest;
use crate::protocol::JsonRpcResponse;
use crate::protocol::LogMessageNotificationParams;
use crate::protocol::McpMethod;
use crate::protocol::ProgressNotificationParams;
use crate::protocol::RequestId;
use crate::protocol::ResourceUpdatedNotificationParams;
use crate::protocol::ServerInfo;
use crate::protocol::Task;
use crate::protocol::latest_supported_protocol_version;
use crate::session::McpSession;
use crate::session::RuntimeCommand;
use crate::session::SharedState;
use crate::transport::MessageTransport;

pub(crate) struct ConnectionOptions {
    pub client_info: Implementation,
    pub capabilities: ClientCapabilities,
    pub handler: Arc<dyn ClientHandler>,
    pub default_timeout: Duration,
}

pub(crate) async fn connect_with_transport(
    transport: Arc<dyn MessageTransport>,
    options: ConnectionOptions,
) -> Result<McpSession, GuestError> {
    let handshake = tokio::time::timeout(
        options.default_timeout,
        perform_handshake(Arc::clone(&transport), &options),
    )
    .await
    .unwrap_or(Err(GuestError::Timeout(options.default_timeout)));
    let info = match handshake {
        Ok(info) => info,
        Err(error) => {
            if let Err(shutdown_error) = transport.shutdown().await {
                tracing::warn!(
                    error = %shutdown_error,
                    "failed to shut down MCP transport after handshake failure"
                );
            }
            return Err(error);
        }
    };
    let shared = Arc::new(SharedState::new(info, options.default_timeout));
    let (command_tx, command_rx) = mpsc::channel(64);

    let runtime_task = tokio::spawn(run_runtime(
        Arc::clone(&transport),
        Arc::clone(&shared),
        Arc::clone(&options.handler),
        command_rx,
    ));

    Ok(McpSession::new(command_tx, shared, transport, runtime_task))
}

async fn perform_handshake(
    transport: Arc<dyn MessageTransport>,
    options: &ConnectionOptions,
) -> Result<ServerInfo, GuestError> {
    let requested_version = latest_supported_protocol_version().to_string();
    let initialize = InitializeRequest {
        protocol_version: requested_version.clone(),
        capabilities: options.capabilities.clone(),
        client_info: options.client_info.clone(),
    };

    transport
        .send(JsonRpcMessage::Request(JsonRpcRequest::new(
            serde_json::json!(crate::protocol::INITIALIZE_REQUEST_ID),
            "initialize",
            Some(serde_json::to_value(&initialize)?),
        )))
        .await?;

    let init_result: InitializeResult = loop {
        match transport.recv().await? {
            JsonRpcMessage::Response(response) => {
                if response.id != Some(serde_json::json!(crate::protocol::INITIALIZE_REQUEST_ID)) {
                    continue;
                }
                if let Some(error) = response.error {
                    return Err(GuestError::server_from_error(error));
                }
                let result = response.result.ok_or_else(|| {
                    GuestError::Protocol("initialize returned no result".to_string())
                })?;
                break serde_json::from_value(result)?;
            }
            JsonRpcMessage::Notification(notification) => {
                dispatch_preinit_notification(notification, Arc::clone(&options.handler)).await;
            }
            JsonRpcMessage::Request(request) => {
                let response =
                    handle_server_request_message(request, Arc::clone(&options.handler)).await;
                transport.send(JsonRpcMessage::Response(response)).await?;
            }
        }
    };

    if !crate::protocol::is_supported_protocol_version(&init_result.protocol_version) {
        return Err(GuestError::VersionMismatch {
            sent: requested_version,
            server: init_result.protocol_version,
        });
    }

    transport
        .send(JsonRpcMessage::Notification(JsonRpcRequest::notification(
            "notifications/initialized",
            None,
        )))
        .await?;

    Ok(ServerInfo {
        server_info: init_result.server_info,
        protocol_version: init_result.protocol_version,
        capabilities: init_result.capabilities,
        instructions: init_result.instructions,
    })
}

async fn run_runtime(
    transport: Arc<dyn MessageTransport>,
    shared: Arc<SharedState>,
    handler: Arc<dyn ClientHandler>,
    mut command_rx: mpsc::Receiver<RuntimeCommand>,
) {
    let (server_response_tx, mut server_response_rx) =
        mpsc::unbounded_channel::<(RequestId, JsonRpcResponse)>();
    let (send_failure_tx, mut send_failure_rx) =
        mpsc::unbounded_channel::<(RequestId, GuestError)>();
    let mut pending_outgoing: HashMap<RequestId, oneshot::Sender<Result<Value, GuestError>>> =
        HashMap::new();
    let mut inbound_requests: HashMap<RequestId, JoinHandle<()>> = HashMap::new();

    loop {
        tokio::select! {
            command = command_rx.recv() => {
                match command {
                    Some(command) => {
                        if handle_runtime_command(
                            Arc::clone(&transport),
                            command,
                            &mut pending_outgoing,
                            &mut inbound_requests,
                            &send_failure_tx,
                        ).await {
                            break;
                        }
                    }
                    None => {
                        fail_pending(&mut pending_outgoing, GuestError::Disconnected);
                        abort_inbound(&mut inbound_requests);
                        if let Err(error) = transport.force_shutdown().await {
                            tracing::error!(
                                %error,
                                "failed to force MCP transport shutdown after session handles were dropped"
                            );
                        }
                        break;
                    }
                }
            }
            Some((request_id, response)) = server_response_rx.recv() => {
                inbound_requests.remove(&request_id);
                if let Err(error) = transport.send(JsonRpcMessage::Response(response)).await {
                    tracing::warn!(error = %error, "failed to send response to server");
                }
            }
            Some((request_id, error)) = send_failure_rx.recv() => {
                if let Some(response_tx) = pending_outgoing.remove(&request_id) {
                    let _ = response_tx.send(Err(error));
                }
            }
            message = transport.recv() => {
                match message {
                    Ok(JsonRpcMessage::Response(response)) => {
                        route_response(response, &mut pending_outgoing);
                    }
                    Ok(JsonRpcMessage::Notification(notification)) => {
                        dispatch_notification(
                            notification,
                            Arc::clone(&shared),
                            Arc::clone(&handler),
                            &mut pending_outgoing,
                            &mut inbound_requests,
                        ).await;
                    }
                    Ok(JsonRpcMessage::Request(request)) => {
                        let Some(id_value) = request.id.clone() else {
                            continue;
                        };
                        let Some(request_id) = RequestId::from_value(&id_value) else {
                            let response = JsonRpcResponse::error(
                                Some(id_value),
                                JsonRpcError::invalid_request("request id must be string or number"),
                            );
                            if let Err(error) = transport.send(JsonRpcMessage::Response(response)).await {
                                tracing::warn!(error = %error, "failed to send invalid-request response");
                            }
                            continue;
                        };

                        let request_id_for_task = request_id.clone();
                        let handler = Arc::clone(&handler);
                        let server_response_tx = server_response_tx.clone();
                        let handle = tokio::spawn(async move {
                            let response = handle_server_request_message(request, handler).await;
                            let _ = server_response_tx.send((request_id_for_task, response));
                        });
                        inbound_requests.insert(request_id, handle);
                    }
                    Err(error) => {
                        tracing::debug!(error = %error, "transport closed");
                        fail_pending(&mut pending_outgoing, GuestError::Disconnected);
                        abort_inbound(&mut inbound_requests);
                        if let Err(shutdown_error) = transport.shutdown().await {
                            tracing::warn!(
                                error = %shutdown_error,
                                "failed to finish MCP transport shutdown after receive failure"
                            );
                        }
                        break;
                    }
                }
            }
        }
    }
}

async fn handle_runtime_command(
    transport: Arc<dyn MessageTransport>,
    command: RuntimeCommand,
    pending_outgoing: &mut HashMap<RequestId, oneshot::Sender<Result<Value, GuestError>>>,
    inbound_requests: &mut HashMap<RequestId, JoinHandle<()>>,
    send_failure_tx: &mpsc::UnboundedSender<(RequestId, GuestError)>,
) -> bool {
    match command {
        RuntimeCommand::Request {
            request_id,
            method,
            params,
            response_tx,
        } => {
            pending_outgoing.insert(request_id.clone(), response_tx);
            let request = JsonRpcRequest::new(request_id.to_value(), method, params);
            let send_failure_tx = send_failure_tx.clone();
            tokio::spawn(async move {
                if let Err(error) = transport.send(JsonRpcMessage::Request(request)).await {
                    let _ = send_failure_tx.send((request_id, error));
                }
            });
            false
        }
        RuntimeCommand::Notification {
            method,
            params,
            response_tx,
        } => {
            let notification = JsonRpcRequest::notification(method, params);
            let result = transport
                .send(JsonRpcMessage::Notification(notification))
                .await;
            let _ = response_tx.send(result);
            false
        }
        RuntimeCommand::Cancel { request_id, reason } => {
            pending_outgoing.remove(&request_id);
            let params = serde_json::to_value(CancelledNotificationParams {
                request_id: Some(request_id),
                reason,
            })
            .ok();
            let notification = JsonRpcRequest::notification("notifications/cancelled", params);
            let _ = transport
                .send(JsonRpcMessage::Notification(notification))
                .await;
            false
        }
        RuntimeCommand::Shutdown { response_tx } => {
            fail_pending(pending_outgoing, GuestError::Disconnected);
            abort_inbound(inbound_requests);
            let result = transport.shutdown().await;
            let _ = response_tx.send(result);
            true
        }
    }
}

fn route_response(
    response: JsonRpcResponse,
    pending_outgoing: &mut HashMap<RequestId, oneshot::Sender<Result<Value, GuestError>>>,
) {
    let Some(request_id) = response.id.as_ref().and_then(RequestId::from_value) else {
        return;
    };

    let Some(response_tx) = pending_outgoing.remove(&request_id) else {
        return;
    };

    let result = if let Some(error) = response.error {
        Err(GuestError::server_from_error(error))
    } else {
        Ok(response.result.unwrap_or_else(|| serde_json::json!({})))
    };

    let _ = response_tx.send(result);
}

async fn dispatch_preinit_notification(
    notification: JsonRpcRequest,
    handler: Arc<dyn ClientHandler>,
) {
    match McpMethod::from(notification.method.as_str()) {
        McpMethod::NotificationsMessage => {
            if let Some(params) = notification.params
                && let Ok(params) = serde_json::from_value::<LogMessageNotificationParams>(params)
            {
                handler.on_log_message(params).await;
            }
        }
        _ => {
            handler
                .on_custom_notification(notification.method, notification.params)
                .await;
        }
    }
}

async fn dispatch_notification(
    notification: JsonRpcRequest,
    shared: Arc<SharedState>,
    handler: Arc<dyn ClientHandler>,
    pending_outgoing: &mut HashMap<RequestId, oneshot::Sender<Result<Value, GuestError>>>,
    inbound_requests: &mut HashMap<RequestId, JoinHandle<()>>,
) {
    match McpMethod::from(notification.method.as_str()) {
        McpMethod::NotificationsCancelled => {
            if let Some(params) = notification.params
                && let Ok(params) = serde_json::from_value::<CancelledNotificationParams>(params)
                && let Some(request_id) = params.request_id
            {
                if let Some(response_tx) = pending_outgoing.remove(&request_id) {
                    let _ = response_tx.send(Err(GuestError::Cancelled));
                }
                if let Some(handle) = inbound_requests.remove(&request_id) {
                    handle.abort();
                }
            }
        }
        McpMethod::NotificationsMessage => {
            if let Some(params) = notification.params
                && let Ok(params) = serde_json::from_value::<LogMessageNotificationParams>(params)
            {
                tokio::spawn(async move {
                    handler.on_log_message(params).await;
                });
            }
        }
        McpMethod::NotificationsProgress => {
            if let Some(params) = notification.params
                && let Ok(params) = serde_json::from_value::<ProgressNotificationParams>(params)
            {
                tokio::spawn(async move {
                    handler.on_progress(params).await;
                });
            }
        }
        McpMethod::NotificationsToolsListChanged => {
            shared.tools.invalidate().await;
            tokio::spawn(async move {
                handler.on_tools_list_changed().await;
            });
        }
        McpMethod::NotificationsResourcesListChanged => {
            shared.resources.invalidate().await;
            shared.resource_templates.invalidate().await;
            tokio::spawn(async move {
                handler.on_resources_list_changed().await;
            });
        }
        McpMethod::NotificationsPromptsListChanged => {
            shared.prompts.invalidate().await;
            tokio::spawn(async move {
                handler.on_prompts_list_changed().await;
            });
        }
        McpMethod::NotificationsRootsListChanged => {
            tokio::spawn(async move {
                handler.on_roots_list_changed().await;
            });
        }
        McpMethod::NotificationsResourcesUpdated => {
            if let Some(params) = notification.params
                && let Ok(params) =
                    serde_json::from_value::<ResourceUpdatedNotificationParams>(params)
            {
                tokio::spawn(async move {
                    handler.on_resource_updated(params).await;
                });
            }
        }
        McpMethod::NotificationsTasksStatus => {
            if let Some(params) = notification.params
                && let Ok(task) = serde_json::from_value::<Task>(params)
            {
                tokio::spawn(async move {
                    handler.on_task_status(task).await;
                });
            }
        }
        McpMethod::NotificationsElicitationComplete => {
            if let Some(params) = notification.params
                && let Ok(params) =
                    serde_json::from_value::<ElicitationCompleteNotificationParams>(params)
            {
                tokio::spawn(async move {
                    handler.on_elicitation_complete(params).await;
                });
            }
        }
        _ => {
            let method = notification.method;
            let params = notification.params;
            tokio::spawn(async move {
                handler.on_custom_notification(method, params).await;
            });
        }
    }
}

async fn handle_server_request_message(
    request: JsonRpcRequest,
    handler: Arc<dyn ClientHandler>,
) -> JsonRpcResponse {
    let id = request
        .id
        .clone()
        .unwrap_or_else(|| serde_json::json!("missing-id"));

    let result = match McpMethod::from(request.method.as_str()) {
        McpMethod::Ping => handler.handle_ping().await,
        McpMethod::RootsList => handler
            .list_roots()
            .await
            .and_then(|roots| serde_json::to_value(roots).map_err(GuestError::from)),
        McpMethod::SamplingCreateMessage => {
            match parse_params::<CreateMessageRequest>(request.params) {
                Ok(params) => handler
                    .create_message(params)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(GuestError::from)),
                Err(error) => Err(error),
            }
        }
        McpMethod::ElicitationCreate => {
            match parse_params::<CreateElicitationRequest>(request.params) {
                Ok(params) => handler
                    .create_elicitation(params)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(GuestError::from)),
                Err(error) => Err(error),
            }
        }
        _ => {
            handler
                .on_custom_request(request.method, request.params)
                .await
        }
    };

    match result {
        Ok(value) => JsonRpcResponse::success(id, value),
        Err(error) => JsonRpcResponse::error(Some(id), guest_error_to_jsonrpc(error)),
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, GuestError> {
    let params = params.ok_or_else(|| GuestError::InvalidParams("missing params".to_string()))?;
    serde_json::from_value(params).map_err(|error| GuestError::InvalidParams(error.to_string()))
}

fn guest_error_to_jsonrpc(error: GuestError) -> JsonRpcError {
    match error {
        GuestError::Json(error) => JsonRpcError::parse_error(error.to_string()),
        GuestError::Server {
            code,
            message,
            data,
        } => JsonRpcError {
            code,
            message,
            data,
        },
        GuestError::InvalidParams(message) => JsonRpcError::invalid_params(message),
        GuestError::MethodNotSupported(method) => {
            JsonRpcError::method_not_found(format!("method not supported: {method}"))
        }
        GuestError::Cancelled => JsonRpcError::new(-32000, "request cancelled"),
        GuestError::Timeout(duration) => {
            JsonRpcError::new(-32000, format!("request timed out after {duration:?}"))
        }
        GuestError::VersionMismatch { sent, server } => JsonRpcError::new(
            -32000,
            format!("protocol version mismatch: sent {sent}, server {server}"),
        ),
        GuestError::UnsupportedProtocolVersion(version) => {
            JsonRpcError::new(-32000, format!("unsupported protocol version: {version}"))
        }
        GuestError::SessionExpired => JsonRpcError::new(-32000, "session expired"),
        other => JsonRpcError::internal_error(other.to_string()),
    }
}

fn fail_pending(
    pending_outgoing: &mut HashMap<RequestId, oneshot::Sender<Result<Value, GuestError>>>,
    error: GuestError,
) {
    for (_, response_tx) in pending_outgoing.drain() {
        let _ = response_tx.send(Err(error.clone_for_fanout()));
    }
}

fn abort_inbound(inbound_requests: &mut HashMap<RequestId, JoinHandle<()>>) {
    for (_, handle) in inbound_requests.drain() {
        handle.abort();
    }
}

#[cfg(test)]
mod tests;
