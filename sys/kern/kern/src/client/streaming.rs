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
use chaos_ipc::protocol::TokenUsage;
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
use chaos_snitch::SessionTelemetry;
use futures::StreamExt;
use rama::http::HeaderMap as ApiHeaderMap;
use rama::http::HeaderValue;
use rama::http::StatusCode;
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
    CLAMP_MCP_ALLOWED_TOOL_RULE, build_clamp_mcp_config, clamp_permission_mode,
    handle_clamp_hook_callback, handle_clamp_mcp_message, handle_clamp_tool_permission,
    render_clamp_full_prompt, render_latest_clamp_user_message,
};
use super::{
    ApiTelemetry, AuthRequestTelemetryContext, HttpTurnRequestConfig, ModelClientSession,
    PendingUnauthorizedRetry, RESPONSES_ENDPOINT, RequestRouteTelemetry,
    UnauthorizedRecoveryExecution, X_CODEX_BETA_FEATURES_HEADER, X_CODEX_TURN_METADATA_HEADER,
    X_CODEX_TURN_STATE_HEADER,
};

// ── Response stream helpers ───────────────────────────────────────────────────

fn clamp_usage_to_token_usage(
    aggregate_usage: chaos_clamp::Usage,
    context_usage: Option<&chaos_clamp::Usage>,
) -> TokenUsage {
    fn saturating_i64(value: u64) -> i64 {
        i64::try_from(value).unwrap_or(i64::MAX)
    }

    let input_tokens = aggregate_usage
        .input_tokens
        .saturating_add(aggregate_usage.cache_creation_input_tokens)
        .saturating_add(aggregate_usage.cache_read_input_tokens);
    // Claude Code's result usage aggregates every provider call in its tool
    // loop. Keep those counters for activity reporting, but derive the context
    // load from the final raw assistant message, whose usage is per-call.
    let total_tokens = context_usage
        .map(|usage| {
            usage
                .input_tokens
                .saturating_add(usage.cache_creation_input_tokens)
                .saturating_add(usage.cache_read_input_tokens)
                .saturating_add(usage.output_tokens)
        })
        // Missing per-call usage is safer as an unknown/zero context load than
        // as the confidently wrong aggregate that drives auto-compaction.
        .unwrap_or(0);

    TokenUsage {
        input_tokens: saturating_i64(input_tokens),
        cache_creation_input_tokens: saturating_i64(aggregate_usage.cache_creation_input_tokens),
        cached_input_tokens: saturating_i64(aggregate_usage.cache_read_input_tokens),
        output_tokens: saturating_i64(aggregate_usage.output_tokens),
        reasoning_output_tokens: 0,
        total_tokens: saturating_i64(total_tokens),
        provider_request_count: 0,
    }
}

fn antigravity_usage_to_token_usage(usage: chaos_clamp::AntigravityUsage) -> TokenUsage {
    fn saturating_i64(value: u64) -> i64 {
        i64::try_from(value).unwrap_or(i64::MAX)
    }

    TokenUsage {
        input_tokens: saturating_i64(usage.input_tokens),
        cache_creation_input_tokens: 0,
        cached_input_tokens: saturating_i64(usage.cache_read_tokens),
        output_tokens: saturating_i64(usage.output_tokens),
        reasoning_output_tokens: saturating_i64(usage.thinking_tokens),
        total_tokens: saturating_i64(usage.total_tokens),
        // The session records provider_request_started separately, so response
        // usage must not count the same subprocess invocation a second time.
        provider_request_count: 0,
    }
}

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
        headers.insert(X_CODEX_BETA_FEATURES_HEADER, header_value);
    }
    if let Some(turn_state) = turn_state
        && let Some(state) = turn_state.get()
        && let Ok(header_value) = HeaderValue::from_str(state)
    {
        headers.insert(X_CODEX_TURN_STATE_HEADER, header_value);
    }
    if let Some(header_value) = turn_metadata_header {
        headers.insert(X_CODEX_TURN_METADATA_HEADER, header_value.clone());
    }
    headers
}

/// Adapt a model adapter's event stream into a `ResponseStream`: convert each
/// `Ok` event into a `ResponseEvent`, map adapter errors to API errors, and
/// pipe the result through [`map_response_stream`].
pub(super) fn adapt_adapter_stream<S, E>(
    api_stream: S,
    session_telemetry: SessionTelemetry,
) -> ResponseStream
where
    S: futures::Stream<Item = std::result::Result<E, AbiError>> + Unpin + Send + 'static,
    E: Send + 'static,
    ResponseEvent: From<E>,
{
    let response_events = api_stream.map(|event| {
        event
            .map(ResponseEvent::from)
            .map_err(abi_error_to_api_error)
    });
    map_response_stream(response_events, session_telemetry)
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
            .map(|manager| manager.for_provider(&self.client.state.provider_id))
            .map(|manager| manager.unauthorized_recovery());
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

    /// Streams a turn via Google's official Antigravity CLI.
    #[instrument(
        name = "model_client.stream_antigravity",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = "clamped",
            transport = "antigravity_subprocess",
        )
    )]
    async fn stream_antigravity(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
    ) -> Result<ResponseStream> {
        use chaos_clamp::AntigravityConfig;
        use chaos_clamp::AntigravityTransport;

        let settings = self.client.state.clamp_settings.antigravity.clone();
        let full_prompt_state = render_clamp_full_prompt(prompt);
        let latest_user_content = render_latest_clamp_user_message(prompt);
        let model = settings
            .model
            .clone()
            .unwrap_or_else(|| antigravity_model_slug(&model_info.slug, effort));
        let clamp_state = Arc::clone(&self.client.state);
        let client = self.client.clone();
        let (tx_event, rx_event) =
            mpsc::channel::<std::result::Result<ResponseEvent, chaos_parrot::error::ApiError>>(256);

        let session_telemetry = session_telemetry.clone();
        tokio::spawn(async move {
            let mut guard = clamp_state.antigravity_transport.lock().await;
            if guard
                .as_ref()
                .is_some_and(|transport| transport.model() != model)
            {
                guard.take();
                clamp_state.clear_antigravity_conversation();
            }
            if guard.is_none() {
                let (bridge_socket_path, bridge_token) =
                    match client.ensure_clamp_mcp_bridge().await {
                        Ok(bridge) => bridge,
                        Err(error) => {
                            let _ = tx_event
                                .send(Err(chaos_parrot::error::ApiError::Stream(error)))
                                .await;
                            return;
                        }
                    };
                let chaos_executable = match std::env::current_exe() {
                    Ok(path) => path,
                    Err(error) => {
                        let _ = tx_event
                            .send(Err(chaos_parrot::error::ApiError::Stream(format!(
                                "failed to resolve Chaos executable for Antigravity bridge: {error}"
                            ))))
                            .await;
                        return;
                    }
                };
                // The subprocess gets a private CA and a loopback proxy; the
                // sandbox then permits only that proxy's port, so an `agy` that
                // ignores the proxy environment reaches nothing.
                let session = clamp_state
                    .session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                // The bundle lands beside the conversation state, or in a
                // directory of our own when there is none. Never the shared
                // temporary root: the session CA is written owner-only, and
                // tightening `/tmp` itself is neither permitted nor desirable.
                let ca_directory = settings.conversation_dir().unwrap_or_else(|| {
                    std::env::temp_dir()
                        .join(format!("chaos-egress-{}", clamp_state.conversation_id))
                });
                if let Err(error) = std::fs::create_dir_all(&ca_directory) {
                    let _ = tx_event
                        .send(Err(chaos_parrot::error::ApiError::Stream(format!(
                            "failed to create Antigravity egress directory: {error}"
                        ))))
                        .await;
                    return;
                }
                let ca_bundle_path =
                    ca_directory.join(format!("egress-ca-{}.pem", clamp_state.conversation_id));
                let egress = match crate::clamp_egress::start_antigravity_egress(
                    clamp_wiretap_sink(clamp_wiretap_mode(), &session),
                    ca_bundle_path,
                )
                .await
                {
                    Ok((proxy, egress)) => {
                        *clamp_state.antigravity_egress.lock().await = Some(proxy);
                        egress
                    }
                    Err(error) => {
                        let _ = tx_event
                            .send(Err(chaos_parrot::error::ApiError::Stream(error)))
                            .await;
                        return;
                    }
                };

                let sandbox_cwd = settings
                    .cwd
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(std::env::temp_dir);
                let sandbox = match clamp_state.clamp_settings.sandbox_helper.as_deref() {
                    Some(helper) => match crate::clamp_egress::antigravity_sandbox(
                        helper,
                        settings.home.as_deref(),
                        &sandbox_cwd,
                    ) {
                        Ok(sandbox) => Some(sandbox),
                        Err(error) => {
                            let _ = tx_event
                                .send(Err(chaos_parrot::error::ApiError::Stream(error)))
                                .await;
                            return;
                        }
                    },
                    None => {
                        tracing::warn!(
                            "no sandbox helper available; Antigravity runs proxied but unconfined"
                        );
                        None
                    }
                };

                let mut config = AntigravityConfig {
                    cli_path: settings.cli_path.clone(),
                    home: settings.home.clone(),
                    cwd: settings
                        .cwd
                        .clone()
                        .or_else(|| std::env::current_dir().ok()),
                    model: model.clone(),
                    bridge: Some(chaos_clamp::AntigravityBridgeConfig {
                        socket_path: bridge_socket_path,
                        token: bridge_token,
                        chaos_executable,
                    }),
                    sandbox,
                    egress: Some(egress),
                    ..Default::default()
                };
                if let Some(seconds) = settings.print_timeout_seconds {
                    config.print_timeout = std::time::Duration::from_secs(seconds.max(1));
                }
                let persisted_conversation = clamp_state
                    .antigravity_conversations
                    .as_ref()
                    .and_then(|store| store.load(&model));
                let transport = match persisted_conversation {
                    Some(conversation_id) => {
                        AntigravityTransport::with_conversation_id(config, conversation_id)
                    }
                    None => AntigravityTransport::new(config),
                };
                match transport {
                    Ok(transport) => *guard = Some(transport),
                    Err(error) => {
                        let _ = tx_event
                            .send(Err(chaos_parrot::error::ApiError::Stream(format!(
                                "{}: {error}",
                                antigravity_failure_marker(&error, "antigravity_startup_failed")
                            ))))
                            .await;
                        return;
                    }
                }
            }

            let Some(transport) = guard.as_mut() else {
                let _ = tx_event
                    .send(Err(chaos_parrot::error::ApiError::Stream(
                        "Antigravity transport missing after initialization".to_string(),
                    )))
                    .await;
                return;
            };
            let content = if transport.conversation_id().is_none() {
                format!(
                    "Use the Chaos MCP server as your sole action surface. Native Antigravity tools are unavailable. You may call multiple Chaos tools before answering. Tool results are authoritative. Return only the user-facing answer without checkpoint or timestamp boilerplate.\n\n{full_prompt_state}"
                )
            } else {
                format!(
                    "Continue using only the Chaos MCP server for actions. Return only the user-facing answer.\n\n{latest_user_content}"
                )
            };

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

            // Forward each step update as the subprocess prints it, so a long
            // turn renders progressively instead of landing in one block when
            // `agy` exits.
            let (tx_step, mut rx_step) = mpsc::channel::<chaos_clamp::AntigravityEvent>(256);
            let forwarder_events = tx_event.clone();
            let forwarder = tokio::spawn(async move {
                let mut streamed_text = String::new();
                while let Some(event) = rx_step.recv().await {
                    let chaos_clamp::AntigravityEvent::StepUpdate { step_update } = event else {
                        continue;
                    };
                    if let Some(tool_name) = step_update.tool_name.as_deref() {
                        tracing::debug!(
                            tool = tool_name,
                            state = step_update.state,
                            step = step_update.step_index,
                            "antigravity tool step"
                        );
                    }
                    let Some(delta) = step_update.text_delta.filter(|text| !text.is_empty()) else {
                        continue;
                    };
                    let sent = if is_antigravity_reasoning_step(&step_update.step_type) {
                        forwarder_events
                            .send(Ok(ResponseEvent::ReasoningContentDelta {
                                delta,
                                content_index: 0,
                            }))
                            .await
                    } else {
                        streamed_text.push_str(&delta);
                        forwarder_events
                            .send(Ok(ResponseEvent::OutputTextDelta(delta)))
                            .await
                    };
                    if sent.is_err() {
                        break;
                    }
                }
                streamed_text
            });

            let turn = transport.run_turn_streamed(&content, Some(&tx_step)).await;
            drop(tx_step);
            let streamed_text = forwarder.await.unwrap_or_default();

            match turn {
                Ok(turn) => {
                    if let Some(store) = clamp_state.antigravity_conversations.as_ref()
                        && let Err(error) = store.save(transport.model(), &turn.conversation_id)
                    {
                        warn!("failed to persist Antigravity conversation state: {error}");
                    }
                    // `agy` repeats the whole answer in its result event; the
                    // deltas already carried it, so only emit what was missed.
                    let response = if turn.response.is_empty() {
                        streamed_text
                    } else {
                        if streamed_text.is_empty() && !turn.response.is_empty() {
                            let _ = tx_event
                                .send(Ok(ResponseEvent::OutputTextDelta(turn.response.clone())))
                                .await;
                        }
                        turn.response
                    };
                    let _ = tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
                            id: None,
                            role: "assistant".to_string(),
                            content: vec![ContentItem::OutputText { text: response }],
                            end_turn: Some(true),
                            phase: None,
                        })))
                        .await;
                    let _ = tx_event
                        .send(Ok(ResponseEvent::Completed {
                            response_id: turn.conversation_id,
                            token_usage: turn.usage.map(antigravity_usage_to_token_usage),
                        }))
                        .await;
                }
                Err(error) => {
                    clamp_state.clear_antigravity_conversation();
                    let _ = tx_event
                        .send(Err(chaos_parrot::error::ApiError::Stream(format!(
                            "{}: {error}",
                            antigravity_failure_marker(&error, "antigravity_runtime_failed")
                        ))))
                        .await;
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx_event);
        Ok(map_response_stream(stream, session_telemetry))
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

                // Opt-in wiretap: when CHAOS_CLAMP_WIRETAP is set, route the
                // subprocess through a loopback recording proxy. Off by default,
                // leaving the subprocess to talk to Anthropic directly.
                let anthropic_base_url = match clamp_wiretap_mode() {
                    WiretapMode::Off => None,
                    mode => {
                        let sink = clamp_wiretap_sink(mode, &session);
                        match chaos_clamp::WiretapProxy::start(sink).await {
                            Ok(proxy) => {
                                let base_url = proxy.base_url();
                                *clamp_state.clamp_wiretap.lock().await = Some(proxy);
                                Some(base_url)
                            }
                            Err(err) => {
                                tracing::warn!("clamp wiretap failed to start: {err}");
                                None
                            }
                        }
                    }
                };

                let config = ClampConfig {
                    system_prompt: Some(system_prompt),
                    permission_mode: Some(clamp_permission_mode(clamp_state.approval_policy)),
                    mcp_config: Some(build_clamp_mcp_config(&bridge_socket_path, &bridge_token)),
                    allow_claude_code_tools: false,
                    allowed_tools: vec![CLAMP_MCP_ALLOWED_TOOL_RULE.to_string()],
                    anthropic_base_url,
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
                                    "{}: {e}",
                                    clamp_failure_marker(&e, "clamp_startup_failed")
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
                                "{}: {e}",
                                clamp_failure_marker(&e, "clamp_startup_failed")
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
                        "{}: {e}",
                        clamp_failure_marker(&e, "clamp_runtime_failed")
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
                        "{}: {e}",
                        clamp_failure_marker(&e, "clamp_runtime_failed")
                    ))))
                    .await;
                return;
            }

            let mut full_text = String::new();
            let mut last_assistant_usage = None;
            loop {
                match transport.next_message().await {
                    Ok(Some(ClampMessage::Assistant { message })) => {
                        if let Some(raw_usage) = message.get("usage").cloned() {
                            match serde_json::from_value::<chaos_clamp::Usage>(raw_usage) {
                                Ok(usage) => last_assistant_usage = Some(usage),
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        "failed to parse clamp assistant usage"
                                    );
                                }
                            }
                        }
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
                    Ok(Some(ClampMessage::Result {
                        session_id, usage, ..
                    })) => {
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
                                token_usage: usage.map(|usage| {
                                    clamp_usage_to_token_usage(usage, last_assistant_usage.as_ref())
                                }),
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
                                "{}: {e}",
                                clamp_failure_marker(&e, "clamp_runtime_failed")
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
        let sniffer = chaos_libration::registry::sniffer_for(
            "anthropic_messages",
            &client_setup.api_provider.base_url,
        );

        let adapter = AnthropicAdapter::new(
            client_setup.api_provider,
            auth,
            Some(model_info.slug.clone()),
        )
        .with_sniffer(sniffer);

        match adapter.stream(turn_request).await {
            Ok(stream) => Ok(adapt_adapter_stream(stream, session_telemetry.clone())),
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
            return match self.client.state.clamp_settings.backend {
                crate::config::ClampBackend::ClaudeCode => {
                    self.stream_clamped(prompt, model_info, session_telemetry)
                        .await
                }
                crate::config::ClampBackend::Antigravity => {
                    self.stream_antigravity(prompt, model_info, session_telemetry, effort)
                        .await
                }
            };
        }

        // Fail fast (and stop hammering provider auth) when no credentials are
        // present. Clamped transport authenticates itself, so it is exempt and
        // handled above. Offline fixture replay needs no credentials, so it
        // bypasses the preflight (the fixture fast path lives inside
        // stream_responses_api).
        if crate::flags::CHAOS_RS_SSE_FIXTURE.is_none() {
            self.client.auth_preflight().await?;
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
                .stream_lsd_api(prompt, model_info, session_telemetry)
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
        match self.client.state.provider.api_key() {
            Ok(Some(api_key)) => return Ok(AnthropicAuth::ApiKey(api_key)),
            Ok(None) => {}
            Err(ChaosErr::EnvVar(_)) => {
                return Err(crate::api_bridge::provider_auth_missing(
                    &self.client.state.provider,
                ));
            }
            Err(other) => return Err(other),
        }

        if let Some(token) = self.client.state.provider.experimental_bearer_token.clone() {
            return Ok(AnthropicAuth::BearerToken(token));
        }

        Err(crate::api_bridge::provider_auth_missing(
            &self.client.state.provider,
        ))
    }

    fn resolve_chat_completions_api_key(&self) -> Result<String> {
        match self.client.state.provider.api_key() {
            Ok(Some(api_key)) => return Ok(api_key),
            Ok(None) => {}
            Err(ChaosErr::EnvVar(_)) => {
                return Err(crate::api_bridge::provider_auth_missing(
                    &self.client.state.provider,
                ));
            }
            Err(other) => return Err(other),
        }

        if let Some(token) = self.client.state.provider.experimental_bearer_token.clone() {
            return Ok(token);
        }

        Err(crate::api_bridge::provider_auth_missing(
            &self.client.state.provider,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_lsd_api",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = "tensorzero",
            transport = "tensorzero_http",
        )
    )]
    async fn stream_lsd_api(
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
        let sniffer = chaos_libration::registry::sniffer_for(
            "tensorzero",
            &client_setup.api_provider.base_url,
        );
        let adapter = chaos_parrot::lsd::LsdAdapter::new(
            client_setup.api_provider,
            api_key,
            Some(model_info.slug.clone()),
        )
        .with_sniffer(sniffer);

        tracing::debug!(
            provider = %self.client.state.provider.name,
            wire_api = "tensorzero",
            "streaming via TensorZero native inference API"
        );

        match adapter.stream(turn_request).await {
            Ok(stream) => Ok(adapt_adapter_stream(stream, session_telemetry.clone())),
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
        let sniffer = chaos_libration::registry::sniffer_for(
            "chat_completions",
            &client_setup.api_provider.base_url,
        );
        let adapter = ChatCompletionsAdapter::new(
            client_setup.api_provider,
            api_key,
            Some(model_info.slug.clone()),
        )
        .with_sniffer(sniffer);

        match adapter.stream(turn_request).await {
            Ok(stream) => Ok(adapt_adapter_stream(stream, session_telemetry.clone())),
            Err(err) => Err(map_api_error(abi_error_to_api_error(err))),
        }
    }
}

fn clamp_failure_marker(error: &chaos_clamp::ClampError, fallback: &'static str) -> &'static str {
    match error {
        chaos_clamp::ClampError::CliNotFound(_) => "clamp_cli_not_found",
        chaos_clamp::ClampError::AuthenticationUnavailable => "clamp_auth_unavailable",
        _ => fallback,
    }
}

fn antigravity_failure_marker(
    error: &chaos_clamp::AntigravityError,
    fallback: &'static str,
) -> &'static str {
    match error {
        chaos_clamp::AntigravityError::CliNotFound(_) => "antigravity_cli_not_found",
        chaos_clamp::AntigravityError::AuthenticationUnavailable => "antigravity_auth_unavailable",
        chaos_clamp::AntigravityError::Timeout => "antigravity_timeout",
        _ => fallback,
    }
}

/// Antigravity reports thinking and answer text through the same field, so the
/// step kind decides which Chaos stream a delta belongs on.
fn is_antigravity_reasoning_step(step_type: &str) -> bool {
    let step_type = step_type.to_ascii_lowercase();
    step_type.contains("thinking") || step_type.contains("reasoning")
}

/// Derives the `agy` model slug from the session model.
///
/// Antigravity slugs carry the reasoning tier as a suffix. A slug that already
/// names its tier is passed through, and anything that is not a Gemini slug is
/// left alone for the CLI to accept or reject. `antigravity.model` in
/// `config.toml` overrides this entirely when Google renames a model.
fn antigravity_model_slug(model: &str, effort: Option<ReasoningEffortConfig>) -> String {
    const TIERS: [&str; 3] = ["low", "medium", "high"];

    let model = model.rsplit('/').next().unwrap_or(model);
    let model = model.strip_suffix("-preview").unwrap_or(model);
    if TIERS
        .iter()
        .any(|tier| model.ends_with(&format!("-{tier}")))
    {
        return model.to_string();
    }
    if !model.starts_with("gemini-") {
        return model.to_string();
    }

    let tier = match effort.unwrap_or(ReasoningEffortConfig::High) {
        ReasoningEffortConfig::None
        | ReasoningEffortConfig::Minimal
        | ReasoningEffortConfig::Low => "low",
        ReasoningEffortConfig::Medium => "medium",
        ReasoningEffortConfig::High
        | ReasoningEffortConfig::XHigh
        | ReasoningEffortConfig::Max
        | ReasoningEffortConfig::Ultra => "high",
    };
    // Pro models expose only the two extremes.
    let tier = if model.contains("-pro") && tier == "medium" {
        "high"
    } else {
        tier
    };
    format!("{model}-{tier}")
}

/// How the clamp wiretap records traffic, resolved from `CHAOS_CLAMP_WIRETAP`.
///
/// - unset/empty → `Off` (the default)
/// - `db` → `Db` (persist to the runtime DB; falls back to tracing if absent)
/// - `1` / `true` / `on` → `File` to a default JSONL in the temp dir
/// - `trace` / `tracing` → `File(None)` (log to the `chaos_clamp::wiretap` target)
/// - any other value → `File` treating the value as the record path
enum WiretapMode {
    Off,
    Db,
    File(Option<std::path::PathBuf>),
}

fn clamp_wiretap_mode() -> WiretapMode {
    let Ok(value) = std::env::var("CHAOS_CLAMP_WIRETAP") else {
        return WiretapMode::Off;
    };
    match value.trim() {
        "" => WiretapMode::Off,
        "db" | "database" => WiretapMode::Db,
        "1" | "true" | "on" => {
            WiretapMode::File(Some(std::env::temp_dir().join("chaos-clamp-wiretap.jsonl")))
        }
        "trace" | "tracing" => WiretapMode::File(None),
        path => WiretapMode::File(Some(std::path::PathBuf::from(path))),
    }
}

/// Builds the recorder backing a clamp proxy. `Off` still yields a sink because
/// the Antigravity egress proxy always runs; it simply records to the
/// `chaos_clamp::wiretap` tracing target, which is silent unless enabled.
fn clamp_wiretap_sink(
    mode: WiretapMode,
    session: &std::sync::Weak<crate::chaos::Session>,
) -> Arc<dyn chaos_clamp::WiretapSink> {
    match mode {
        WiretapMode::Db => {
            let upgraded = session.upgrade();
            match upgraded.as_ref().and_then(|session| session.runtime_db()) {
                Some(db) => {
                    let session_id = upgraded
                        .as_ref()
                        .map(|session| session.conversation_id.to_string());
                    Arc::new(crate::clamp_wiretap::DbWiretapSink::new(db, session_id))
                }
                None => {
                    tracing::warn!("clamp wiretap=db but no runtime db; logging to tracing");
                    Arc::new(chaos_clamp::FileWiretapSink::new(None))
                }
            }
        }
        WiretapMode::File(path) => Arc::new(chaos_clamp::FileWiretapSink::new(path)),
        WiretapMode::Off => Arc::new(chaos_clamp::FileWiretapSink::new(None)),
    }
}

#[cfg(test)]
mod tests {
    use super::antigravity_model_slug;
    use super::antigravity_usage_to_token_usage;
    use super::clamp_usage_to_token_usage;
    use chaos_clamp::AntigravityUsage;
    use chaos_clamp::Usage;
    use chaos_ipc::openai_models::ReasoningEffort;

    #[test]
    fn antigravity_model_mapping_uses_observed_cli_slugs() {
        assert_eq!(
            antigravity_model_slug("gemini-3.1-pro-preview", Some(ReasoningEffort::Low)),
            "gemini-3.1-pro-low"
        );
        assert_eq!(
            antigravity_model_slug("google/gemini-3.1-pro", Some(ReasoningEffort::Medium)),
            "gemini-3.1-pro-high"
        );
        assert_eq!(
            antigravity_model_slug("gemini-3.6-flash", Some(ReasoningEffort::Medium)),
            "gemini-3.6-flash-medium"
        );
        assert_eq!(
            antigravity_model_slug("gemini-3.6-flash-low", Some(ReasoningEffort::High)),
            "gemini-3.6-flash-low"
        );
        assert_eq!(
            antigravity_model_slug("claude-opus-5", Some(ReasoningEffort::Low)),
            "claude-opus-5"
        );
        assert!(super::is_antigravity_reasoning_step("thinking_delta"));
        assert!(super::is_antigravity_reasoning_step("MODEL_REASONING"));
        assert!(!super::is_antigravity_reasoning_step("text_delta"));
    }

    #[test]
    fn antigravity_usage_preserves_reasoning_and_cache_tokens() {
        let usage = antigravity_usage_to_token_usage(AntigravityUsage {
            input_tokens: 11,
            output_tokens: 13,
            thinking_tokens: 17,
            cache_read_tokens: 19,
            total_tokens: 60,
        });

        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.cached_input_tokens, 19);
        assert_eq!(usage.output_tokens, 13);
        assert_eq!(usage.reasoning_output_tokens, 17);
        assert_eq!(usage.total_tokens, 60);
        assert_eq!(usage.provider_request_count, 0);
    }

    #[test]
    fn clamp_usage_uses_aggregate_counters_and_last_call_context() {
        let usage = clamp_usage_to_token_usage(
            Usage {
                input_tokens: 8,
                cache_creation_input_tokens: 45_558,
                cache_read_input_tokens: 136_189,
                output_tokens: 233,
            },
            Some(&Usage {
                input_tokens: 3,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 45_700,
                output_tokens: 47,
            }),
        );

        assert_eq!(usage.input_tokens, 181_755);
        assert_eq!(usage.cache_creation_input_tokens, 45_558);
        assert_eq!(usage.cached_input_tokens, 136_189);
        assert_eq!(usage.output_tokens, 233);
        assert_eq!(usage.reasoning_output_tokens, 0);
        assert_eq!(usage.total_tokens, 45_750);
        assert_eq!(usage.provider_request_count, 0);
    }

    #[test]
    fn clamp_usage_saturates_on_overflow() {
        let max_usage = Usage {
            input_tokens: u64::MAX,
            cache_creation_input_tokens: u64::MAX,
            cache_read_input_tokens: u64::MAX,
            output_tokens: u64::MAX,
        };
        let usage = clamp_usage_to_token_usage(max_usage.clone(), Some(&max_usage));

        assert_eq!(usage.input_tokens, i64::MAX);
        assert_eq!(usage.cache_creation_input_tokens, i64::MAX);
        assert_eq!(usage.cached_input_tokens, i64::MAX);
        assert_eq!(usage.output_tokens, i64::MAX);
        assert_eq!(usage.reasoning_output_tokens, 0);
        assert_eq!(usage.total_tokens, i64::MAX);
        assert_eq!(usage.provider_request_count, 0);
    }

    #[test]
    fn clamp_usage_leaves_context_unknown_without_assistant_usage() {
        let usage = clamp_usage_to_token_usage(
            Usage {
                input_tokens: 11,
                cache_creation_input_tokens: 13,
                cache_read_input_tokens: 17,
                output_tokens: 19,
            },
            None,
        );

        assert_eq!(usage.input_tokens, 41);
        assert_eq!(usage.output_tokens, 19);
        assert_eq!(usage.total_tokens, 0);
    }
}
