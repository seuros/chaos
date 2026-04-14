//! `ModelClientSession` streaming implementation — one turn per session.
//!
//! Covers the Responses API (HTTP), Anthropic Messages API, Chat Completions
//! API, TensorZero native API, and the clamped Claude Code subprocess path.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use chaos_abi::AbiError;
use chaos_abi::FreeformToolDef;
use chaos_abi::FunctionToolDef;
use chaos_abi::ModelAdapter;
use chaos_abi::ReasoningConfig as AbiReasoningConfig;
use chaos_abi::ToolDef as AbiToolDef;
use chaos_abi::TurnRequest as AbiTurnRequest;
use chaos_ipc::config_types::ReasoningSummary as ReasoningSummaryConfig;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_parrot::RamaTransport;
use chaos_parrot::RequestTelemetry;
use chaos_parrot::ResponsesOptions as ApiResponsesOptions;
use chaos_parrot::SseTelemetry;
use chaos_parrot::TransportError;
use chaos_parrot::anthropic::AnthropicAdapter;
use chaos_parrot::anthropic::AnthropicAuth;
use chaos_parrot::chat_completions::ChatCompletionsAdapter;
use chaos_parrot::openai::OpenAiAdapter;
use chaos_parrot::requests::responses::Compression;
use chaos_syslog::SessionTelemetry;
use futures::StreamExt;
use http::HeaderMap as ApiHeaderMap;
use http::HeaderValue;
use http::StatusCode;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::instrument;
use tracing::warn;

use crate::api_bridge::abi_error_to_api_error;
use crate::api_bridge::map_api_error;
use crate::auth::ChaosAuth;
use crate::auth::RefreshTokenError;
use crate::auth::UnauthorizedRecovery;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::client_common::ResponseStream;
use crate::client_common::tools::ToolSpec;
use crate::error::ChaosErr;
use crate::error::Result;
use crate::model_provider_info::WireApi;
use crate::response_debug_context::extract_response_debug_context;
use crate::tools::spec::create_tools_json_for_responses_api;
use crate::util::emit_feedback_auth_recovery_tags;

use super::tools::{
    build_clamp_mcp_config, clamp_permission_mode, handle_clamp_hook_callback,
    handle_clamp_mcp_message, handle_clamp_tool_permission, render_clamp_full_prompt,
    render_latest_clamp_user_message,
};
use super::{
    ApiTelemetry, AuthRequestTelemetryContext, HttpTurnRequestConfig, ModelClientSession,
    PendingUnauthorizedRetry, RESPONSES_ENDPOINT, RequestRouteTelemetry,
    UnauthorizedRecoveryExecution, X_CHAOS_TURN_METADATA_HEADER, X_CHAOS_TURN_STATE_HEADER,
};

// ── Response stream helpers ───────────────────────────────────────────────────

/// Parses per-turn metadata into an HTTP header value.
pub(super) fn parse_turn_metadata_header(
    turn_metadata_header: Option<&str>,
) -> Option<HeaderValue> {
    turn_metadata_header.and_then(|value| HeaderValue::from_str(value).ok())
}

pub(super) fn tool_spec_to_abi_tool(tool: &ToolSpec) -> Option<AbiToolDef> {
    match tool {
        ToolSpec::Function(tool) => Some(AbiToolDef::Function(FunctionToolDef {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: serde_json::to_value(&tool.parameters).ok()?,
            strict: tool.strict,
        })),
        ToolSpec::Freeform(tool) => Some(AbiToolDef::Freeform(FreeformToolDef {
            name: tool.name.clone(),
            description: tool.description.clone(),
            format_type: tool.format.r#type.clone(),
            syntax: tool.format.syntax.clone(),
            definition: tool.format.definition.clone(),
        })),
        _ => None,
    }
}

/// Builds the extra headers attached to Responses API requests.
pub(super) fn build_responses_headers(
    beta_features_header: Option<&str>,
    turn_state: Option<&Arc<std::sync::OnceLock<String>>>,
    turn_metadata_header: Option<&HeaderValue>,
) -> ApiHeaderMap {
    let mut headers = ApiHeaderMap::new();
    if let Some(value) = beta_features_header
        && !value.is_empty()
        && let Ok(header_value) = HeaderValue::from_str(value)
    {
        headers.insert("x-chaos-beta-features", header_value);
    }
    if let Some(turn_state) = turn_state
        && let Some(state) = turn_state.get()
        && let Ok(header_value) = HeaderValue::from_str(state)
    {
        headers.insert(X_CHAOS_TURN_STATE_HEADER, header_value);
    }
    if let Some(header_value) = turn_metadata_header {
        headers.insert(X_CHAOS_TURN_METADATA_HEADER, header_value.clone());
    }
    headers
}

pub(super) fn map_response_stream<S>(
    api_stream: S,
    session_telemetry: SessionTelemetry,
) -> ResponseStream
where
    S: futures::Stream<Item = std::result::Result<ResponseEvent, chaos_parrot::error::ApiError>>
        + Unpin
        + Send
        + 'static,
{
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent>>(1600);

    tokio::spawn(async move {
        let mut logged_error = false;
        let mut api_stream = api_stream;
        while let Some(event) = api_stream.next().await {
            match event {
                Ok(ResponseEvent::OutputItemDone(item)) => {
                    if tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(item)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(ResponseEvent::Completed {
                    response_id,
                    token_usage,
                }) => {
                    if let Some(usage) = &token_usage {
                        session_telemetry.sse_event_completed(
                            usage.input_tokens,
                            usage.output_tokens,
                            Some(usage.cached_input_tokens),
                            Some(usage.reasoning_output_tokens),
                            usage.total_tokens,
                        );
                    }
                    if tx_event
                        .send(Ok(ResponseEvent::Completed {
                            response_id,
                            token_usage,
                        }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(event) => {
                    if tx_event.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let mapped = map_api_error(err);
                    if !logged_error {
                        session_telemetry.see_event_completed_failed(&mapped);
                        logged_error = true;
                    }
                    if tx_event.send(Err(mapped)).await.is_err() {
                        return;
                    }
                }
            }
        }
    });

    ResponseStream { rx_event }
}

pub(super) async fn handle_unauthorized(
    transport: TransportError,
    auth_recovery: &mut Option<UnauthorizedRecovery>,
    session_telemetry: &SessionTelemetry,
) -> Result<UnauthorizedRecoveryExecution> {
    let debug = extract_response_debug_context(&transport);
    if let Some(recovery) = auth_recovery
        && recovery.has_next()
    {
        let mode = recovery.mode_name();
        let phase = recovery.step_name();
        return match recovery.next().await {
            Ok(step_result) => {
                session_telemetry.record_auth_recovery(
                    mode,
                    phase,
                    "recovery_succeeded",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                    /*recovery_reason*/ None,
                    step_result.auth_state_changed(),
                );
                emit_feedback_auth_recovery_tags(
                    mode,
                    phase,
                    "recovery_succeeded",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                );
                Ok(UnauthorizedRecoveryExecution { mode, phase })
            }
            Err(RefreshTokenError::Permanent(failed)) => {
                session_telemetry.record_auth_recovery(
                    mode,
                    phase,
                    "recovery_failed_permanent",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                    /*recovery_reason*/ None,
                    /*auth_state_changed*/ None,
                );
                emit_feedback_auth_recovery_tags(
                    mode,
                    phase,
                    "recovery_failed_permanent",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                );
                Err(ChaosErr::RefreshTokenFailed(failed))
            }
            Err(RefreshTokenError::Transient(other)) => {
                session_telemetry.record_auth_recovery(
                    mode,
                    phase,
                    "recovery_failed_transient",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                    /*recovery_reason*/ None,
                    /*auth_state_changed*/ None,
                );
                emit_feedback_auth_recovery_tags(
                    mode,
                    phase,
                    "recovery_failed_transient",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                );
                Err(ChaosErr::Io(other))
            }
        };
    }

    let (mode, phase, recovery_reason) = match auth_recovery.as_ref() {
        Some(recovery) => (
            recovery.mode_name(),
            recovery.step_name(),
            Some(recovery.unavailable_reason()),
        ),
        None => ("none", "none", Some("auth_manager_missing")),
    };
    session_telemetry.record_auth_recovery(
        mode,
        phase,
        "recovery_not_run",
        debug.request_id.as_deref(),
        debug.cf_ray.as_deref(),
        debug.auth_error.as_deref(),
        debug.auth_error_code.as_deref(),
        recovery_reason,
        /*auth_state_changed*/ None,
    );
    emit_feedback_auth_recovery_tags(
        mode,
        phase,
        "recovery_not_run",
        debug.request_id.as_deref(),
        debug.cf_ray.as_deref(),
        debug.auth_error.as_deref(),
        debug.auth_error_code.as_deref(),
    );

    Err(map_api_error(chaos_parrot::error::ApiError::Transport(
        transport,
    )))
}

// ── ModelClientSession impl ───────────────────────────────────────────────────

impl ModelClientSession {
    fn build_http_turn_request(
        &self,
        provider: &chaos_parrot::Provider,
        prompt: &Prompt,
        model_info: &ModelInfo,
        config: HttpTurnRequestConfig<'_>,
    ) -> Result<AbiTurnRequest> {
        let input = prompt.get_formatted_input();
        let mut openai_tools = create_tools_json_for_responses_api(&prompt.tools)?;
        for tool_name in &model_info.native_server_side_tools {
            openai_tools.push(serde_json::json!({"type": tool_name}));
        }
        let tools = prompt
            .tools
            .iter()
            .filter_map(tool_spec_to_abi_tool)
            .collect::<Vec<_>>();
        let verbosity = if model_info.support_verbosity {
            self.client
                .state
                .model_verbosity
                .or(model_info.default_verbosity)
        } else {
            if self.client.state.model_verbosity.is_some() {
                warn!(
                    "model_verbosity is set but ignored as the model does not support verbosity: {}",
                    model_info.slug
                );
            }
            None
        };
        let reasoning = if model_info.supports_reasoning_summaries {
            Some(AbiReasoningConfig {
                effort: config.effort.or(model_info.default_reasoning_level),
                summary: if config.summary == ReasoningSummaryConfig::None {
                    None
                } else {
                    Some(config.summary)
                },
            })
        } else {
            None
        };

        let mut request_headers = serde_json::Map::new();
        for (name, value) in &config.options.extra_headers {
            if let Ok(value) = value.to_str() {
                request_headers.insert(name.as_str().to_string(), json!(value));
            }
        }

        let mut extensions = serde_json::Map::new();
        extensions.insert(
            "store".to_string(),
            json!(provider.is_azure_responses_endpoint()),
        );
        extensions.insert(
            "prompt_cache_key".to_string(),
            json!(self.client.state.conversation_id.to_string()),
        );
        extensions.insert(
            "openai_tools".to_string(),
            serde_json::Value::Array(openai_tools),
        );
        extensions.insert(
            "request_headers".to_string(),
            serde_json::Value::Object(request_headers),
        );
        extensions.insert(
            "compression".to_string(),
            json!(match config.options.compression {
                Compression::None => "none",
                Compression::Zstd => "zstd",
            }),
        );
        if let Some(service_tier) = match config.service_tier {
            Some(ServiceTier::Fast) => Some("priority".to_string()),
            Some(other) => Some(other.to_string()),
            None => None,
        } {
            extensions.insert("service_tier".to_string(), json!(service_tier));
        }

        Ok(AbiTurnRequest {
            model: model_info.slug.clone(),
            instructions: prompt.base_instructions.text.clone(),
            input,
            tools,
            parallel_tool_calls: prompt.parallel_tool_calls,
            reasoning,
            output_schema: prompt.output_schema.clone(),
            verbosity,
            turn_state: config.options.turn_state.clone(),
            extensions,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_responses_options(
        &self,
        turn_metadata_header: Option<&str>,
        compression: Compression,
    ) -> ApiResponsesOptions {
        let turn_metadata_header = parse_turn_metadata_header(turn_metadata_header);
        let conversation_id = self.client.state.conversation_id.to_string();
        let mut extra_headers = crate::default_client::default_headers();
        extra_headers.extend(build_responses_headers(
            self.client.state.beta_features_header.as_deref(),
            Some(&self.turn_state),
            turn_metadata_header.as_ref(),
        ));
        ApiResponsesOptions {
            conversation_id: Some(conversation_id),
            session_source: Some(self.client.state.session_source.clone()),
            extra_headers,
            compression,
            turn_state: Some(Arc::clone(&self.turn_state)),
        }
    }

    fn responses_request_compression(&self, auth: Option<&crate::auth::ChaosAuth>) -> Compression {
        if self.client.state.enable_request_compression
            && auth.is_some_and(ChaosAuth::is_chatgpt_auth)
            && self.client.state.provider.is_openai()
        {
            Compression::Zstd
        } else {
            Compression::None
        }
    }

    /// Builds request and SSE telemetry for streaming API calls.
    fn build_streaming_telemetry(
        session_telemetry: &SessionTelemetry,
        auth_context: AuthRequestTelemetryContext,
        request_route_telemetry: RequestRouteTelemetry,
    ) -> (Arc<dyn RequestTelemetry>, Arc<dyn SseTelemetry>) {
        let telemetry = Arc::new(ApiTelemetry::new(
            session_telemetry.clone(),
            auth_context,
            request_route_telemetry,
        ));
        let request_telemetry: Arc<dyn RequestTelemetry> = telemetry.clone();
        let sse_telemetry: Arc<dyn SseTelemetry> = telemetry;
        (request_telemetry, sse_telemetry)
    }

    /// Streams a turn via the OpenAI Responses API (HTTP/SSE).
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_responses_api",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = %self.client.state.provider.wire_api,
            transport = "responses_http",
            http.method = "POST",
            api.path = "responses",
            turn.has_metadata_header = turn_metadata_header.is_some()
        )
    )]
    async fn stream_responses_api(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<ServiceTier>,
        turn_metadata_header: Option<&str>,
    ) -> Result<ResponseStream> {
        if let Some(path) = &*crate::flags::CHAOS_RS_SSE_FIXTURE {
            warn!(path, "Streaming from fixture");
            let stream = chaos_parrot::stream_from_fixture(
                path,
                self.client.state.provider.stream_idle_timeout(),
            )
            .map_err(map_api_error)?;
            let stream = map_response_stream(stream, session_telemetry.clone());
            return Ok(stream);
        }

        let auth_manager = self.client.state.auth_manager.clone();
        let mut auth_recovery = auth_manager
            .as_ref()
            .map(crate::auth::AuthManager::unauthorized_recovery);
        let mut pending_retry = PendingUnauthorizedRetry::default();
        loop {
            let client_setup = self.client.current_client_setup().await?;
            let provider_for_errors = client_setup.api_provider.clone();
            let transport = RamaTransport::default_client();
            let request_auth_context = AuthRequestTelemetryContext::new(
                client_setup.auth.as_ref().map(ChaosAuth::auth_mode),
                &client_setup.api_auth,
                pending_retry,
            );
            let (request_telemetry, sse_telemetry) = Self::build_streaming_telemetry(
                session_telemetry,
                request_auth_context,
                RequestRouteTelemetry::for_endpoint(RESPONSES_ENDPOINT),
            );
            let compression = self.responses_request_compression(client_setup.auth.as_ref());
            let options = self.build_responses_options(turn_metadata_header, compression);
            let turn_request = self.build_http_turn_request(
                &client_setup.api_provider,
                prompt,
                model_info,
                HttpTurnRequestConfig {
                    effort,
                    summary,
                    service_tier,
                    options: &options,
                },
            )?;
            let adapter = OpenAiAdapter::new(
                transport,
                client_setup.api_provider,
                client_setup.api_auth,
                Some(model_info.slug.clone()),
                self.client.state.representer.clone(),
            )
            .with_options(options.clone())
            .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
            let stream_result = adapter.stream(turn_request).await;

            match stream_result {
                Ok(stream) => {
                    let response_events = stream.map(|event| {
                        event
                            .map(ResponseEvent::from)
                            .map_err(abi_error_to_api_error)
                    });
                    let stream = map_response_stream(response_events, session_telemetry.clone());
                    return Ok(stream);
                }
                Err(AbiError::Transport { status, message })
                    if status == StatusCode::UNAUTHORIZED.as_u16() =>
                {
                    let unauthorized_transport = TransportError::Http {
                        status: StatusCode::UNAUTHORIZED,
                        url: Some(provider_for_errors.url_for_path("responses")),
                        headers: None,
                        body: Some(message),
                    };
                    pending_retry = PendingUnauthorizedRetry::from_recovery(
                        handle_unauthorized(
                            unauthorized_transport,
                            &mut auth_recovery,
                            session_telemetry,
                        )
                        .await?,
                    );
                    continue;
                }
                Err(err) => return Err(map_api_error(abi_error_to_api_error(err))),
            }
        }
    }

    /// Streams a turn via a clamped Claude Code subprocess.
    #[instrument(
        name = "model_client.stream_clamped",
        level = "info",
        skip_all,
        fields(
            // model_info.slug is the outer session model; clamp routes to
            // Claude Code MAX which picks its own model.  Use "clamp" so
            // traces are not misleadingly attributed to the outer slug.
            model = "clamp",
            wire_api = "clamped",
            transport = "claude_subprocess",
        )
    )]
    async fn stream_clamped(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
    ) -> Result<ResponseStream> {
        use chaos_clamp::ClampConfig;
        use chaos_clamp::ClampTransport;
        use chaos_clamp::Message as ClampMessage;
        let system_prompt = prompt.base_instructions.text.clone();
        let full_prompt_state = render_clamp_full_prompt(prompt);
        let latest_user_content = render_latest_clamp_user_message(prompt);
        let clamp_model_slug = model_info.slug.clone();
        let client = self.client.clone();

        let clamp_state = Arc::clone(&self.client.state);

        let (tx_event, rx_event) =
            mpsc::channel::<std::result::Result<ResponseEvent, chaos_parrot::error::ApiError>>(256);

        let session_telemetry = session_telemetry.clone();
        tokio::spawn(async move {
            let mut guard = clamp_state.clamp_transport.lock().await;
            let mut spawned_fresh = false;
            let session = clamp_state
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();

            if guard.is_none() {
                let permission_session = session.clone();
                let hook_session = session.clone();
                let mcp_session = session.clone();
                let (bridge_socket_path, bridge_token) =
                    match client.ensure_clamp_mcp_bridge().await {
                        Ok(bridge) => bridge,
                        Err(err) => {
                            let _ = tx_event
                                .send(Err(chaos_parrot::error::ApiError::Stream(err)))
                                .await;
                            return;
                        }
                    };
                let config = ClampConfig {
                    system_prompt: Some(system_prompt),
                    permission_mode: Some(clamp_permission_mode(clamp_state.approval_policy)),
                    mcp_config: Some(build_clamp_mcp_config(&bridge_socket_path, &bridge_token)),
                    allow_claude_code_tools: false,
                    tool_permission_handler: Some(Arc::new(
                        move |tool_name, input, tool_use_id| {
                            let session = permission_session.clone();
                            Box::pin(async move {
                                handle_clamp_tool_permission(session, tool_name, input, tool_use_id)
                                    .await
                            })
                        },
                    )),
                    hook_callback_handler: Some(Arc::new(
                        move |callback_id, input, tool_use_id| {
                            let session = hook_session.clone();
                            Box::pin(async move {
                                handle_clamp_hook_callback(session, callback_id, input, tool_use_id)
                                    .await
                            })
                        },
                    )),
                    mcp_message_handler: Some(Arc::new(move |server_name, message| {
                        let session = mcp_session.clone();
                        Box::pin(async move {
                            handle_clamp_mcp_message(session, server_name, message).await
                        })
                    })),
                    ..Default::default()
                };
                match ClampTransport::spawn(config).await {
                    Ok(mut t) => {
                        if let Err(e) = t.initialize().await {
                            let _ = tx_event
                                .send(Err(chaos_parrot::error::ApiError::Stream(format!(
                                    "clamp init failed: {e}"
                                ))))
                                .await;
                            return;
                        }
                        if let Some(models) =
                            t.init_response().and_then(|r| r.get("models").cloned())
                        {
                            chaos_clamp::set_cached_models(models);
                        }
                        spawned_fresh = true;
                        *guard = Some(t);
                    }
                    Err(e) => {
                        let _ = tx_event
                            .send(Err(chaos_parrot::error::ApiError::Stream(format!(
                                "clamp spawn failed: {e}"
                            ))))
                            .await;
                        return;
                    }
                }
            }

            let Some(transport) = guard.as_mut() else {
                let _ = tx_event
                    .send(Err(chaos_parrot::error::ApiError::Stream(
                        "clamp transport missing after initialization".to_string(),
                    )))
                    .await;
                return;
            };

            // Only override the model when running a Claude model slug.
            // Non-Claude slugs (OpenAI, xAI, …) are not valid in Claude Code;
            // in that case let the subprocess use its MAX-subscription default.
            if clamp_model_slug.starts_with("claude")
                && let Err(e) = transport.set_model(&clamp_model_slug).await
            {
                *guard = None;
                let _ = tx_event
                    .send(Err(chaos_parrot::error::ApiError::Stream(format!(
                        "clamp set_model failed: {e}"
                    ))))
                    .await;
                return;
            }

            let _ = tx_event.send(Ok(ResponseEvent::Created)).await;

            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![],
                    end_turn: None,
                    phase: None,
                })))
                .await;

            let content = if spawned_fresh {
                full_prompt_state.as_str()
            } else {
                latest_user_content.as_str()
            };

            if let Err(e) = transport.send_user_message(content).await {
                *guard = None;
                let _ = tx_event
                    .send(Err(chaos_parrot::error::ApiError::Stream(format!(
                        "clamp send failed: {e}"
                    ))))
                    .await;
                return;
            }

            let mut full_text = String::new();
            loop {
                match transport.next_message().await {
                    Ok(Some(ClampMessage::Assistant { message })) => {
                        if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
                            for block in content {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    full_text.push_str(text);
                                    let _ = tx_event
                                        .send(Ok(ResponseEvent::OutputTextDelta(text.to_string())))
                                        .await;
                                }
                            }
                        }
                    }
                    Ok(Some(ClampMessage::Result { session_id, .. })) => {
                        let _ = tx_event
                            .send(Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
                                id: None,
                                role: "assistant".to_string(),
                                content: vec![ContentItem::OutputText { text: full_text }],
                                end_turn: Some(true),
                                phase: None,
                            })))
                            .await;
                        let response_id = session_id.unwrap_or_else(|| "clamped".to_string());
                        let _ = tx_event
                            .send(Ok(ResponseEvent::Completed {
                                response_id,
                                token_usage: None,
                            }))
                            .await;
                        break;
                    }
                    Ok(Some(ClampMessage::System { .. })) => {}
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        *guard = None;
                        let _ = tx_event
                            .send(Ok(ResponseEvent::Completed {
                                response_id: "clamped-eof".to_string(),
                                token_usage: None,
                            }))
                            .await;
                        break;
                    }
                    Err(e) => {
                        *guard = None;
                        let _ = tx_event
                            .send(Err(chaos_parrot::error::ApiError::Stream(format!(
                                "clamp error: {e}"
                            ))))
                            .await;
                        break;
                    }
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx_event);
        let response_stream = map_response_stream(stream, session_telemetry);
        Ok(response_stream)
    }

    /// Streams a turn via the Anthropic Messages API.
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_anthropic_messages",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = "anthropic_messages",
            transport = "anthropic_http",
        )
    )]
    async fn stream_anthropic_messages(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<ServiceTier>,
    ) -> Result<ResponseStream> {
        let client_setup = self.client.current_client_setup().await?;

        let options = self.build_responses_options(None, Compression::None);
        let turn_request = self.build_http_turn_request(
            &client_setup.api_provider,
            prompt,
            model_info,
            HttpTurnRequestConfig {
                effort,
                summary,
                service_tier,
                options: &options,
            },
        )?;

        let auth = self.resolve_anthropic_auth()?;

        let adapter = AnthropicAdapter::new(
            client_setup.api_provider,
            auth,
            Some(model_info.slug.clone()),
        );

        match adapter.stream(turn_request).await {
            Ok(stream) => {
                let response_events = stream.map(|event| {
                    event
                        .map(ResponseEvent::from)
                        .map_err(abi_error_to_api_error)
                });
                let stream = map_response_stream(response_events, session_telemetry.clone());
                Ok(stream)
            }
            Err(err) => Err(map_api_error(abi_error_to_api_error(err))),
        }
    }

    fn is_wire_format_mismatch(err: &ChaosErr) -> bool {
        match err {
            ChaosErr::UnexpectedStatus(e) => matches!(e.status.as_u16(), 404 | 405 | 501),
            _ => false,
        }
    }

    /// Streams a single model request within the current turn.
    #[allow(clippy::too_many_arguments)]
    pub async fn stream(
        &mut self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<ServiceTier>,
        turn_metadata_header: Option<&str>,
    ) -> Result<ResponseStream> {
        tracing::debug!(
            provider = %self.client.state.provider.name,
            model = %model_info.slug,
            tool_count = prompt.tools.len(),
            reasoning_effort = ?effort,
            clamped = self.client.state.clamped.load(Ordering::Relaxed),
            "sending model request",
        );

        if self.client.state.clamped.load(Ordering::Relaxed) {
            return self
                .stream_clamped(prompt, model_info, session_telemetry)
                .await;
        }

        if crate::model_provider_info::is_anthropic_wire(
            self.client.state.provider.base_url.as_deref(),
        ) {
            return self
                .stream_anthropic_messages(
                    prompt,
                    model_info,
                    session_telemetry,
                    effort,
                    summary,
                    service_tier,
                )
                .await;
        }

        if self.client.state.provider.wire_api == WireApi::TensorZero {
            return self
                .stream_tensorzero_api(prompt, model_info, session_telemetry)
                .await;
        }

        if self.client.state.provider.wire_api == WireApi::ChatCompletions {
            return self
                .stream_chat_completions_api(
                    prompt,
                    model_info,
                    session_telemetry,
                    effort,
                    summary,
                    service_tier,
                    turn_metadata_header,
                )
                .await;
        }

        if self.client.state.provider.wire_api == WireApi::Auto {
            if let Some(&resolved) = self.client.state.resolved_wire.get() {
                return match resolved {
                    WireApi::ChatCompletions => {
                        self.stream_chat_completions_api(
                            prompt,
                            model_info,
                            session_telemetry,
                            effort,
                            summary,
                            service_tier,
                            turn_metadata_header,
                        )
                        .await
                    }
                    _ => {
                        self.stream_responses_api(
                            prompt,
                            model_info,
                            session_telemetry,
                            effort,
                            summary,
                            service_tier,
                            turn_metadata_header,
                        )
                        .await
                    }
                };
            }

            match self
                .stream_responses_api(
                    prompt,
                    model_info,
                    session_telemetry,
                    effort,
                    summary,
                    service_tier,
                    turn_metadata_header,
                )
                .await
            {
                Ok(stream) => {
                    let _ = self.client.state.resolved_wire.set(WireApi::Responses);
                    return Ok(stream);
                }
                Err(ref probe_err) if Self::is_wire_format_mismatch(probe_err) => {
                    tracing::debug!(
                        provider = %self.client.state.provider.name,
                        "Responses API probe returned endpoint-not-found; \
                         falling back to Chat Completions"
                    );
                }
                Err(err) => return Err(err),
            }

            let result = self
                .stream_chat_completions_api(
                    prompt,
                    model_info,
                    session_telemetry,
                    effort,
                    summary,
                    service_tier,
                    turn_metadata_header,
                )
                .await;
            if result.is_ok() {
                let _ = self
                    .client
                    .state
                    .resolved_wire
                    .set(WireApi::ChatCompletions);
            }
            return result;
        }

        self.stream_responses_api(
            prompt,
            model_info,
            session_telemetry,
            effort,
            summary,
            service_tier,
            turn_metadata_header,
        )
        .await
    }

    pub(super) fn resolve_anthropic_auth(&self) -> Result<AnthropicAuth> {
        if let Some(api_key) = self.client.state.provider.api_key()? {
            return Ok(AnthropicAuth::ApiKey(api_key));
        }

        if let Some(token) = self.client.state.provider.experimental_bearer_token.clone() {
            return Ok(AnthropicAuth::BearerToken(token));
        }

        Err(ChaosErr::InvalidRequest(format!(
            "Anthropic Messages provider `{}` requires `env_key` or `experimental_bearer_token`",
            self.client.state.provider.name
        )))
    }

    fn resolve_chat_completions_api_key(&self) -> Result<String> {
        if let Some(api_key) = self.client.state.provider.api_key()? {
            return Ok(api_key);
        }

        if let Some(token) = self.client.state.provider.experimental_bearer_token.clone() {
            return Ok(token);
        }

        Err(ChaosErr::InvalidRequest(format!(
            "Chat Completions provider `{}` requires `env_key` or `experimental_bearer_token`",
            self.client.state.provider.name
        )))
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_tensorzero_api",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = "tensorzero",
            transport = "tensorzero_http",
        )
    )]
    async fn stream_tensorzero_api(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
    ) -> Result<ResponseStream> {
        let client_setup = self.client.current_client_setup().await?;
        let options = self.build_responses_options(None, Compression::None);
        let turn_request = self.build_http_turn_request(
            &client_setup.api_provider,
            prompt,
            model_info,
            HttpTurnRequestConfig {
                effort: None,
                summary: ReasoningSummaryConfig::None,
                service_tier: None,
                options: &options,
            },
        )?;

        let api_key = self.resolve_chat_completions_api_key().unwrap_or_default();
        let adapter = chaos_parrot::tensorzero::TensorZeroAdapter::new(
            client_setup.api_provider,
            api_key,
            Some(model_info.slug.clone()),
        );

        tracing::debug!(
            provider = %self.client.state.provider.name,
            wire_api = "tensorzero",
            "streaming via TensorZero native inference API"
        );

        match adapter.stream(turn_request).await {
            Ok(stream) => {
                let response_events = stream.map(|event| {
                    event
                        .map(ResponseEvent::from)
                        .map_err(abi_error_to_api_error)
                });
                let stream = map_response_stream(response_events, session_telemetry.clone());
                Ok(stream)
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    model = %model_info.slug,
                    "TensorZero adapter stream failed"
                );
                Err(map_api_error(abi_error_to_api_error(err)))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_chat_completions_api(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<ServiceTier>,
        turn_metadata_header: Option<&str>,
    ) -> Result<ResponseStream> {
        let client_setup = self.client.current_client_setup().await?;
        let options = self.build_responses_options(turn_metadata_header, Compression::None);
        let turn_request = self.build_http_turn_request(
            &client_setup.api_provider,
            prompt,
            model_info,
            HttpTurnRequestConfig {
                effort,
                summary,
                service_tier,
                options: &options,
            },
        )?;

        let api_key = self.resolve_chat_completions_api_key()?;
        let adapter = ChatCompletionsAdapter::new(
            client_setup.api_provider,
            api_key,
            Some(model_info.slug.clone()),
        );

        match adapter.stream(turn_request).await {
            Ok(stream) => {
                let response_events = stream.map(|event| {
                    event
                        .map(ResponseEvent::from)
                        .map_err(abi_error_to_api_error)
                });
                let stream = map_response_stream(response_events, session_telemetry.clone());
                Ok(stream)
            }
            Err(err) => Err(map_api_error(abi_error_to_api_error(err))),
        }
    }
}
