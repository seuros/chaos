use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
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
    let info = perform_handshake(Arc::clone(&transport), &options).await?;
    let shared = Arc::new(SharedState::new(info, options.default_timeout));
    let next_id = Arc::new(AtomicU64::new(2));
    let (command_tx, command_rx) = mpsc::channel(64);

    tokio::spawn(run_runtime(
        transport,
        Arc::clone(&shared),
        Arc::clone(&options.handler),
        command_rx,
    ));

    Ok(McpSession {
        command_tx,
        shared,
        next_id,
    })
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
            serde_json::json!(1),
            "initialize",
            Some(serde_json::to_value(&initialize)?),
        )))
        .await?;

    let init_result: InitializeResult = loop {
        match transport.recv().await? {
            JsonRpcMessage::Response(response) => {
                if response.id != serde_json::json!(1) {
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
    let mut pending_outgoing: HashMap<RequestId, oneshot::Sender<Result<Value, GuestError>>> =
        HashMap::new();
    let mut inbound_requests: HashMap<RequestId, JoinHandle<()>> = HashMap::new();

    loop {
        tokio::select! {
            Some(command) = command_rx.recv() => {
                if handle_runtime_command(
                    Arc::clone(&transport),
                    command,
                    &mut pending_outgoing,
                    &mut inbound_requests,
                ).await {
                    break;
                }
            }
            Some((request_id, response)) = server_response_rx.recv() => {
                inbound_requests.remove(&request_id);
                if let Err(error) = transport.send(JsonRpcMessage::Response(response)).await {
                    tracing::warn!(error = %error, "failed to send response to server");
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
                                id_value,
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
                        let _ = transport.shutdown().await;
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
            if let Err(error) = transport.send(JsonRpcMessage::Request(request)).await
                && let Some(response_tx) = pending_outgoing.remove(&request_id)
            {
                let _ = response_tx.send(Err(error));
            }
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
            let _ = transport.shutdown().await;
            let _ = response_tx.send(());
            true
        }
    }
}

fn route_response(
    response: JsonRpcResponse,
    pending_outgoing: &mut HashMap<RequestId, oneshot::Sender<Result<Value, GuestError>>>,
) {
    let Some(request_id) = RequestId::from_value(&response.id) else {
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
            *shared.tools.write().await = None;
            tokio::spawn(async move {
                handler.on_tools_list_changed().await;
            });
        }
        McpMethod::NotificationsResourcesListChanged => {
            *shared.resources.write().await = None;
            *shared.resource_templates.write().await = None;
            tokio::spawn(async move {
                handler.on_resources_list_changed().await;
            });
        }
        McpMethod::NotificationsPromptsListChanged => {
            *shared.prompts.write().await = None;
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
            let params = match request.params.clone() {
                Some(params) => params,
                None => {
                    return JsonRpcResponse::error(
                        id,
                        JsonRpcError::invalid_params("missing params"),
                    );
                }
            };
            match serde_json::from_value::<CreateMessageRequest>(params) {
                Ok(params) => handler
                    .create_message(params)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(GuestError::from)),
                Err(error) => Err(GuestError::InvalidParams(error.to_string())),
            }
        }
        McpMethod::ElicitationCreate => {
            let params = match request.params.clone() {
                Some(params) => params,
                None => {
                    return JsonRpcResponse::error(
                        id,
                        JsonRpcError::invalid_params("missing params"),
                    );
                }
            };
            match serde_json::from_value::<CreateElicitationRequest>(params) {
                Ok(params) => handler
                    .create_elicitation(params)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(GuestError::from)),
                Err(error) => Err(GuestError::InvalidParams(error.to_string())),
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
        Err(error) => JsonRpcResponse::error(id, guest_error_to_jsonrpc(error)),
    }
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
        let _ = response_tx.send(Err(match &error {
            GuestError::Disconnected => GuestError::Disconnected,
            GuestError::Cancelled => GuestError::Cancelled,
            GuestError::SessionExpired => GuestError::SessionExpired,
            GuestError::Timeout(duration) => GuestError::Timeout(*duration),
            GuestError::InvalidParams(message) => GuestError::InvalidParams(message.clone()),
            GuestError::MethodNotSupported(method) => {
                GuestError::MethodNotSupported(method.clone())
            }
            GuestError::Protocol(message) => GuestError::Protocol(message.clone()),
            GuestError::Http(message) => GuestError::Http(message.clone()),
            GuestError::UrlParse(message) => GuestError::UrlParse(message.clone()),
            GuestError::UnsupportedProtocolVersion(version) => {
                GuestError::UnsupportedProtocolVersion(version.clone())
            }
            GuestError::VersionMismatch { sent, server } => GuestError::VersionMismatch {
                sent: sent.clone(),
                server: server.clone(),
            },
            GuestError::Server {
                code,
                message,
                data,
            } => GuestError::Server {
                code: *code,
                message: message.clone(),
                data: data.clone(),
            },
            GuestError::Transport(io) => GuestError::Http(io.to_string()),
            GuestError::Json(json) => GuestError::Protocol(json.to_string()),
        }));
    }
}

fn abort_inbound(inbound_requests: &mut HashMap<RequestId, JoinHandle<()>>) {
    for (_, handle) in inbound_requests.drain() {
        handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use tokio::sync::Mutex;

    use super::*;
    use crate::handler::NoopClientHandler;
    use crate::protocol::Implementation;
    use crate::protocol::ServerCapabilities;
    use crate::transport::TransportFuture;

    struct MockTransport {
        sent: Mutex<Vec<JsonRpcMessage>>,
        incoming: Mutex<VecDeque<JsonRpcMessage>>,
    }

    impl MockTransport {
        fn new(incoming: Vec<JsonRpcMessage>) -> Arc<Self> {
            Arc::new(Self {
                sent: Mutex::new(Vec::new()),
                incoming: Mutex::new(VecDeque::from(incoming)),
            })
        }
    }

    impl MessageTransport for MockTransport {
        fn send<'a>(&'a self, message: JsonRpcMessage) -> TransportFuture<'a, ()> {
            Box::pin(async move {
                self.sent.lock().await.push(message);
                Ok(())
            })
        }

        fn recv<'a>(&'a self) -> TransportFuture<'a, JsonRpcMessage> {
            Box::pin(async move {
                self.incoming
                    .lock()
                    .await
                    .pop_front()
                    .ok_or(GuestError::Disconnected)
            })
        }

        fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn handshake_accepts_previous_version_and_replies_to_ping() {
        let transport = MockTransport::new(vec![
            JsonRpcMessage::Request(JsonRpcRequest::new(
                serde_json::json!("ping-1"),
                "ping",
                Some(serde_json::json!({})),
            )),
            JsonRpcMessage::Response(JsonRpcResponse::success(
                serde_json::json!(1),
                serde_json::to_value(InitializeResult {
                    protocol_version: "2025-06-18".to_string(),
                    capabilities: ServerCapabilities::default(),
                    server_info: Implementation::new("example-server", "1.0.0"),
                    instructions: None,
                })
                .unwrap(),
            )),
        ]);

        let info = perform_handshake(
            Arc::clone(&transport) as Arc<dyn MessageTransport>,
            &ConnectionOptions {
                client_info: Implementation::new("test-client", "1.0.0"),
                capabilities: ClientCapabilities::default(),
                handler: Arc::new(NoopClientHandler),
                default_timeout: Duration::from_secs(30),
            },
        )
        .await
        .unwrap();

        assert_eq!(info.protocol_version, "2025-06-18");

        let sent = transport.sent.lock().await;
        assert_eq!(sent.len(), 3);
        assert!(
            matches!(&sent[0], JsonRpcMessage::Request(request) if request.method == "initialize")
        );
        assert!(
            matches!(&sent[1], JsonRpcMessage::Response(response) if response.id == serde_json::json!("ping-1"))
        );
        assert!(
            matches!(&sent[2], JsonRpcMessage::Notification(notification) if notification.method == "notifications/initialized")
        );
    }
}
