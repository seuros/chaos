use std::cmp::Ordering;
use std::collections::HashMap;

use codex_arg0::Arg0DispatchPaths;
use codex_core::AuthManager;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_core::default_client::USER_AGENT_SUFFIX;
use codex_core::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::Submission;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResult;
use rmcp::model::ClientNotification;
use rmcp::model::ClientRequest;
use rmcp::model::EmptyResult;
use rmcp::model::ErrorCode;
use rmcp::model::ErrorData;
use rmcp::model::Implementation;
use rmcp::model::InitializeResult;
use rmcp::model::JsonRpcError;
use rmcp::model::JsonRpcNotification;
use rmcp::model::JsonRpcRequest;
use rmcp::model::JsonRpcResponse;
use rmcp::model::ProtocolVersion;
use rmcp::model::RequestId;
use rmcp::model::ServerCapabilities;
use rmcp::model::ToolsCapability;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task;

use crate::codex_tool_config::CodexToolCallParam;
use crate::codex_tool_config::CodexToolCallReplyParam;
use crate::codex_tool_config::create_tool_for_codex_tool_call_param;
use crate::codex_tool_config::create_tool_for_codex_tool_call_reply_param;
use crate::outgoing_message::OutgoingMessageSender;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleState {
    Uninitialized,
    InitializeResponded,
    Operational,
}

pub(crate) struct MessageProcessor {
    outgoing: Arc<OutgoingMessageSender>,
    lifecycle_state: LifecycleState,
    arg0_paths: Arg0DispatchPaths,
    thread_manager: Arc<ThreadManager>,
    running_requests_id_to_codex_uuid: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
}

impl MessageProcessor {
    /// Create a new `MessageProcessor`, retaining a handle to the outgoing
    /// `Sender` so handlers can enqueue messages to be written to stdout.
    pub(crate) fn new(
        outgoing: OutgoingMessageSender,
        arg0_paths: Arg0DispatchPaths,
        config: Arc<Config>,
    ) -> Self {
        let outgoing = Arc::new(outgoing);
        let auth_manager = AuthManager::shared(
            config.codex_home.clone(),
            /*enable_codex_api_key_env*/ false,
            config.cli_auth_credentials_store_mode,
        );
        let thread_manager = Arc::new(ThreadManager::new(
            config.as_ref(),
            auth_manager,
            SessionSource::Mcp,
            CollaborationModesConfig {
                default_mode_request_user_input: config
                    .features
                    .enabled(codex_core::features::Feature::DefaultModeRequestUserInput),
            },
        ));
        Self {
            outgoing,
            lifecycle_state: LifecycleState::Uninitialized,
            arg0_paths,
            thread_manager,
            running_requests_id_to_codex_uuid: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn process_request(&mut self, request: JsonRpcRequest<ClientRequest>) {
        let request_id = request.id.clone();
        let client_request = request.request;
        let method = match &client_request {
            ClientRequest::InitializeRequest(_) => "initialize",
            ClientRequest::PingRequest(_) => "ping",
            ClientRequest::ListResourcesRequest(_) => "resources/list",
            ClientRequest::ListResourceTemplatesRequest(_) => "resources/templates/list",
            ClientRequest::ReadResourceRequest(_) => "resources/read",
            ClientRequest::SubscribeRequest(_) => "resources/subscribe",
            ClientRequest::UnsubscribeRequest(_) => "resources/unsubscribe",
            ClientRequest::ListPromptsRequest(_) => "prompts/list",
            ClientRequest::GetPromptRequest(_) => "prompts/get",
            ClientRequest::ListToolsRequest(_) => "tools/list",
            ClientRequest::CallToolRequest(_) => "tools/call",
            ClientRequest::SetLevelRequest(_) => "logging/setLevel",
            ClientRequest::CompleteRequest(_) => "completion/complete",
            ClientRequest::GetTaskInfoRequest(_) => "tasks/get_info",
            ClientRequest::ListTasksRequest(_) => "tasks/list",
            ClientRequest::GetTaskResultRequest(_) => "tasks/get_result",
            ClientRequest::CancelTaskRequest(_) => "tasks/cancel",
            ClientRequest::CustomRequest(custom) => custom.method.as_str(),
        };

        match self.lifecycle_state {
            LifecycleState::Uninitialized => {
                if !matches!(
                    &client_request,
                    ClientRequest::InitializeRequest(_) | ClientRequest::PingRequest(_)
                ) {
                    self.outgoing
                        .send_error(
                            request_id,
                            ErrorData::invalid_request(
                                format!("request `{method}` is not allowed before initialize"),
                                Some(json!({ "method": method })),
                            ),
                        )
                        .await;
                    return;
                }
            }
            LifecycleState::InitializeResponded => {
                if matches!(&client_request, ClientRequest::InitializeRequest(_)) {
                    self.outgoing
                        .send_error(
                            request_id,
                            ErrorData::invalid_request("initialize called more than once", None),
                        )
                        .await;
                    return;
                }
                if !matches!(
                    &client_request,
                    ClientRequest::PingRequest(_) | ClientRequest::SetLevelRequest(_)
                ) {
                    self.outgoing
                        .send_error(
                            request_id,
                            ErrorData::invalid_request(
                                format!(
                                    "request `{method}` is not allowed before initialized notification"
                                ),
                                Some(json!({ "method": method })),
                            ),
                        )
                        .await;
                    return;
                }
            }
            LifecycleState::Operational => {
                if matches!(&client_request, ClientRequest::InitializeRequest(_)) {
                    self.outgoing
                        .send_error(
                            request_id,
                            ErrorData::invalid_request("initialize called more than once", None),
                        )
                        .await;
                    return;
                }
            }
        }

        match client_request {
            ClientRequest::InitializeRequest(params) => {
                self.handle_initialize(request_id, params.params).await;
            }
            ClientRequest::PingRequest(_params) => {
                self.handle_ping(request_id).await;
            }
            ClientRequest::ListResourcesRequest(params) => {
                self.handle_list_resources(request_id, params.params).await;
            }
            ClientRequest::ListResourceTemplatesRequest(params) => {
                self.handle_list_resource_templates(request_id, params.params)
                    .await;
            }
            ClientRequest::ReadResourceRequest(params) => {
                self.handle_read_resource(request_id, params.params).await;
            }
            ClientRequest::SubscribeRequest(params) => {
                self.handle_subscribe(request_id, params.params).await;
            }
            ClientRequest::UnsubscribeRequest(params) => {
                self.handle_unsubscribe(request_id, params.params).await;
            }
            ClientRequest::ListPromptsRequest(params) => {
                self.handle_list_prompts(request_id, params.params).await;
            }
            ClientRequest::GetPromptRequest(params) => {
                self.handle_get_prompt(request_id, params.params).await;
            }
            ClientRequest::ListToolsRequest(params) => {
                self.handle_list_tools(request_id, params.params).await;
            }
            ClientRequest::CallToolRequest(params) => {
                self.handle_call_tool(request_id, params.params).await;
            }
            ClientRequest::SetLevelRequest(params) => {
                self.handle_set_level(request_id, params.params).await;
            }
            ClientRequest::CompleteRequest(params) => {
                self.handle_complete(request_id, params.params).await;
            }
            ClientRequest::GetTaskInfoRequest(_) => {
                self.handle_unsupported_request(request_id, "tasks/get_info")
                    .await;
            }
            ClientRequest::ListTasksRequest(_) => {
                self.handle_unsupported_request(request_id, "tasks/list")
                    .await;
            }
            ClientRequest::GetTaskResultRequest(_) => {
                self.handle_unsupported_request(request_id, "tasks/get_result")
                    .await;
            }
            ClientRequest::CancelTaskRequest(_) => {
                self.handle_unsupported_request(request_id, "tasks/cancel")
                    .await;
            }
            ClientRequest::CustomRequest(custom) => {
                let method = custom.method.clone();
                self.outgoing
                    .send_error(
                        request_id,
                        ErrorData::new(
                            ErrorCode::METHOD_NOT_FOUND,
                            format!("method not found: {method}"),
                            Some(json!({ "method": method })),
                        ),
                    )
                    .await;
            }
        }
    }

    pub(crate) async fn process_response(&mut self, response: JsonRpcResponse<serde_json::Value>) {
        tracing::info!("<- response: {:?}", response);
        let JsonRpcResponse { id, result, .. } = response;
        self.outgoing.notify_client_response(id, result).await
    }

    pub(crate) async fn process_notification(
        &mut self,
        notification: JsonRpcNotification<ClientNotification>,
    ) {
        match notification.notification {
            ClientNotification::CancelledNotification(params) => {
                self.handle_cancelled_notification(params.params).await;
            }
            ClientNotification::ProgressNotification(params) => {
                self.handle_progress_notification(params.params);
            }
            ClientNotification::RootsListChangedNotification(_params) => {
                self.handle_roots_list_changed();
            }
            ClientNotification::InitializedNotification(_) => {
                self.handle_initialized_notification();
            }
            ClientNotification::CustomNotification(_) => {
                tracing::warn!("ignoring custom client notification");
            }
        }
    }

    pub(crate) async fn process_error(&mut self, err: JsonRpcError) {
        tracing::error!("<- error: {:?}", err);
        self.outgoing.notify_client_error(err.id, err.error).await;
    }

    async fn handle_initialize(
        &mut self,
        id: RequestId,
        params: rmcp::model::InitializeRequestParams,
    ) {
        tracing::info!("initialize -> params: {:?}", params);

        if self.lifecycle_state != LifecycleState::Uninitialized {
            self.outgoing
                .send_error(
                    id,
                    ErrorData::invalid_request("initialize called more than once", None),
                )
                .await;
            return;
        }

        self.outgoing
            .set_client_elicitation_capability(params.capabilities.elicitation.as_ref());

        let client_info = params.client_info;
        let name = client_info.name;
        let version = client_info.version;
        let user_agent_suffix = format!("{name}; {version}");
        if let Ok(mut suffix) = USER_AGENT_SUFFIX.lock() {
            *suffix = Some(user_agent_suffix);
        }

        let server_info = Implementation {
            name: "codex-mcp-server".to_string(),
            title: Some("Codex".to_string()),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: None,
            icons: None,
            website_url: None,
        };

        let protocol_version = match params
            .protocol_version
            .partial_cmp(&ProtocolVersion::V_2025_06_18)
        {
            Some(Ordering::Less) => params.protocol_version,
            Some(Ordering::Equal | Ordering::Greater) | None => ProtocolVersion::V_2025_06_18,
        };

        let result = InitializeResult {
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(true),
                }),
                ..Default::default()
            },
            instructions: None,
            protocol_version,
            server_info,
        };

        self.lifecycle_state = LifecycleState::InitializeResponded;
        self.outgoing.send_response(id, result).await;
    }

    async fn handle_ping(&self, id: RequestId) {
        tracing::info!("ping");
        self.outgoing.send_response(id, EmptyResult {}).await;
    }

    async fn handle_list_resources(
        &self,
        id: RequestId,
        params: Option<rmcp::model::PaginatedRequestParams>,
    ) {
        tracing::info!("resources/list -> params: {:?}", params);
        self.handle_unsupported_request(id, "resources/list").await;
    }

    async fn handle_list_resource_templates(
        &self,
        id: RequestId,
        params: Option<rmcp::model::PaginatedRequestParams>,
    ) {
        tracing::info!("resources/templates/list -> params: {:?}", params);
        self.handle_unsupported_request(id, "resources/templates/list")
            .await;
    }

    async fn handle_read_resource(
        &self,
        id: RequestId,
        params: rmcp::model::ReadResourceRequestParams,
    ) {
        tracing::info!("resources/read -> params: {:?}", params);
        self.handle_unsupported_request(id, "resources/read").await;
    }

    async fn handle_subscribe(&self, id: RequestId, params: rmcp::model::SubscribeRequestParams) {
        tracing::info!("resources/subscribe -> params: {:?}", params);
        self.handle_unsupported_request(id, "resources/subscribe")
            .await;
    }

    async fn handle_unsubscribe(
        &self,
        id: RequestId,
        params: rmcp::model::UnsubscribeRequestParams,
    ) {
        tracing::info!("resources/unsubscribe -> params: {:?}", params);
        self.handle_unsupported_request(id, "resources/unsubscribe")
            .await;
    }

    async fn handle_list_prompts(
        &self,
        id: RequestId,
        params: Option<rmcp::model::PaginatedRequestParams>,
    ) {
        tracing::info!("prompts/list -> params: {:?}", params);
        self.handle_unsupported_request(id, "prompts/list").await;
    }

    async fn handle_get_prompt(&self, id: RequestId, params: rmcp::model::GetPromptRequestParams) {
        tracing::info!("prompts/get -> params: {:?}", params);
        self.handle_unsupported_request(id, "prompts/get").await;
    }

    async fn handle_list_tools(
        &self,
        id: RequestId,
        params: Option<rmcp::model::PaginatedRequestParams>,
    ) {
        tracing::trace!("tools/list -> {params:?}");
        let result = rmcp::model::ListToolsResult {
            meta: None,
            tools: vec![
                create_tool_for_codex_tool_call_param(),
                create_tool_for_codex_tool_call_reply_param(),
            ],
            next_cursor: None,
        };

        self.outgoing.send_response(id, result).await;
    }

    async fn handle_call_tool(&self, id: RequestId, params: CallToolRequestParams) {
        tracing::info!("tools/call -> params: {:?}", params);
        let CallToolRequestParams {
            name, arguments, ..
        } = params;

        match name.as_ref() {
            "codex" => self.handle_tool_call_codex(id, arguments).await,
            "codex-reply" => {
                self.handle_tool_call_codex_session_reply(id, arguments)
                    .await
            }
            _ => {
                let result = CallToolResult {
                    content: vec![rmcp::model::Content::text(format!("Unknown tool '{name}'"))],
                    structured_content: None,
                    is_error: Some(true),
                    meta: None,
                };
                self.outgoing.send_response(id, result).await;
            }
        }
    }

    async fn handle_tool_call_codex(
        &self,
        id: RequestId,
        arguments: Option<rmcp::model::JsonObject>,
    ) {
        let arguments = arguments.map(serde_json::Value::Object);
        let (initial_prompt, config): (String, Config) = match arguments {
            Some(json_val) => match serde_json::from_value::<CodexToolCallParam>(json_val) {
                Ok(tool_cfg) => match tool_cfg.into_config(self.arg0_paths.clone()).await {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        let result = CallToolResult {
                            content: vec![rmcp::model::Content::text(format!(
                                "Failed to load Codex configuration from overrides: {e}"
                            ))],
                            structured_content: None,
                            is_error: Some(true),
                            meta: None,
                        };
                        self.outgoing.send_response(id, result).await;
                        return;
                    }
                },
                Err(e) => {
                    let result = CallToolResult {
                        content: vec![rmcp::model::Content::text(format!(
                            "Failed to parse configuration for Codex tool: {e}"
                        ))],
                        structured_content: None,
                        is_error: Some(true),
                        meta: None,
                    };
                    self.outgoing.send_response(id, result).await;
                    return;
                }
            },
            None => {
                let result = CallToolResult {
                    content: vec![rmcp::model::Content::text(
                        "Missing arguments for codex tool-call; the `prompt` field is required.",
                    )],
                    structured_content: None,
                    is_error: Some(true),
                    meta: None,
                };
                self.outgoing.send_response(id, result).await;
                return;
            }
        };

        // Clone outgoing and server to move into async task.
        let outgoing = self.outgoing.clone();
        let thread_manager = self.thread_manager.clone();
        let running_requests_id_to_codex_uuid = self.running_requests_id_to_codex_uuid.clone();

        // Spawn an async task to handle the Codex session so that we do not
        // block the synchronous message-processing loop.
        task::spawn(async move {
            // Run the Codex session and stream events back to the client.
            crate::codex_tool_runner::run_codex_tool_session(
                id,
                initial_prompt,
                config,
                outgoing,
                thread_manager,
                running_requests_id_to_codex_uuid,
            )
            .await;
        });
    }

    async fn handle_tool_call_codex_session_reply(
        &self,
        request_id: RequestId,
        arguments: Option<rmcp::model::JsonObject>,
    ) {
        let arguments = arguments.map(serde_json::Value::Object);
        tracing::info!("tools/call -> params: {:?}", arguments);

        // parse arguments
        let codex_tool_call_reply_param: CodexToolCallReplyParam = match arguments {
            Some(json_val) => match serde_json::from_value::<CodexToolCallReplyParam>(json_val) {
                Ok(params) => params,
                Err(e) => {
                    tracing::error!("Failed to parse Codex tool call reply parameters: {e}");
                    let result = CallToolResult {
                        content: vec![rmcp::model::Content::text(format!(
                            "Failed to parse configuration for Codex tool: {e}"
                        ))],
                        structured_content: None,
                        is_error: Some(true),
                        meta: None,
                    };
                    self.outgoing.send_response(request_id, result).await;
                    return;
                }
            },
            None => {
                tracing::error!(
                    "Missing arguments for codex-reply tool-call; the `thread_id` and `prompt` fields are required."
                );
                let result = CallToolResult {
                    content: vec![rmcp::model::Content::text(
                        "Missing arguments for codex-reply tool-call; the `thread_id` and `prompt` fields are required.",
                    )],
                    structured_content: None,
                    is_error: Some(true),
                    meta: None,
                };
                self.outgoing.send_response(request_id, result).await;
                return;
            }
        };

        let thread_id = match codex_tool_call_reply_param.get_thread_id() {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Failed to parse thread_id: {e}");
                let result = CallToolResult {
                    content: vec![rmcp::model::Content::text(format!(
                        "Failed to parse thread_id: {e}"
                    ))],
                    structured_content: None,
                    is_error: Some(true),
                    meta: None,
                };
                self.outgoing.send_response(request_id, result).await;
                return;
            }
        };

        // Clone outgoing to move into async task.
        let outgoing = self.outgoing.clone();
        let running_requests_id_to_codex_uuid = self.running_requests_id_to_codex_uuid.clone();

        let codex = match self.thread_manager.get_thread(thread_id).await {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("Session not found for thread_id: {thread_id}");
                let result = crate::codex_tool_runner::create_call_tool_result_with_thread_id(
                    thread_id,
                    format!("Session not found for thread_id: {thread_id}"),
                    Some(true),
                );
                outgoing.send_response(request_id, result).await;
                return;
            }
        };

        // Spawn the long-running reply handler.
        let prompt = codex_tool_call_reply_param.prompt.clone();
        tokio::spawn({
            let outgoing = outgoing.clone();
            let running_requests_id_to_codex_uuid = running_requests_id_to_codex_uuid.clone();

            async move {
                crate::codex_tool_runner::run_codex_tool_session_reply(
                    thread_id,
                    codex,
                    outgoing,
                    request_id,
                    prompt,
                    running_requests_id_to_codex_uuid,
                )
                .await;
            }
        });
    }

    async fn handle_set_level(&self, id: RequestId, params: rmcp::model::SetLevelRequestParams) {
        tracing::info!("logging/setLevel -> params: {:?}", params);
        self.handle_unsupported_request(id, "logging/setLevel")
            .await;
    }

    async fn handle_complete(&self, id: RequestId, params: rmcp::model::CompleteRequestParams) {
        tracing::info!("completion/complete -> params: {:?}", params);
        self.handle_unsupported_request(id, "completion/complete")
            .await;
    }

    async fn handle_unsupported_request(&self, id: RequestId, method: &str) {
        self.outgoing
            .send_error(
                id,
                ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    format!("method not found: {method}"),
                    Some(json!({ "method": method })),
                ),
            )
            .await;
    }

    // ---------------------------------------------------------------------
    // Notification handlers
    // ---------------------------------------------------------------------

    async fn handle_cancelled_notification(&self, params: rmcp::model::CancelledNotificationParam) {
        let request_id = params.request_id;
        // Create a stable string form early for logging and submission id.
        let request_id_string = request_id.to_string();

        // Obtain the thread id while holding the first lock, then release.
        let thread_id = {
            let map_guard = self.running_requests_id_to_codex_uuid.lock().await;
            match map_guard.get(&request_id) {
                Some(id) => *id,
                None => {
                    tracing::warn!("Session not found for request_id: {request_id_string}");
                    return;
                }
            }
        };
        tracing::info!("thread_id: {thread_id}");

        // Obtain the Codex thread from the server.
        let codex_arc = match self.thread_manager.get_thread(thread_id).await {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("Session not found for thread_id: {thread_id}");
                return;
            }
        };

        // Submit interrupt to Codex.
        if let Err(e) = codex_arc
            .submit_with_id(Submission {
                id: request_id_string,
                op: codex_protocol::protocol::Op::Interrupt,
                trace: None,
            })
            .await
        {
            tracing::error!("Failed to submit interrupt to Codex: {e}");
            return;
        }
        // unregister the id so we don't keep it in the map
        self.running_requests_id_to_codex_uuid
            .lock()
            .await
            .remove(&request_id);
    }

    fn handle_progress_notification(&self, params: rmcp::model::ProgressNotificationParam) {
        tracing::info!("notifications/progress -> params: {:?}", params);
    }

    fn handle_roots_list_changed(&self) {
        tracing::info!("notifications/roots/list_changed");
    }

    fn handle_initialized_notification(&mut self) {
        match self.lifecycle_state {
            LifecycleState::InitializeResponded => {
                tracing::info!("notifications/initialized");
                self.lifecycle_state = LifecycleState::Operational;
            }
            LifecycleState::Uninitialized => {
                tracing::warn!("ignoring notifications/initialized before initialize");
            }
            LifecycleState::Operational => {
                tracing::warn!("ignoring duplicate notifications/initialized");
            }
        }
    }
}
