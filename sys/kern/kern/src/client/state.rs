//! `ModelClient` session-state management: construction, clamp toggling, and
//! unary compact requests.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::Ordering;

use chaos_ipc::config_types::ReasoningSummary as ReasoningSummaryConfig;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::protocol::SessionSource;
use chaos_parrot::CompactClient as ApiCompactClient;
use chaos_parrot::CompactionInput as ApiCompactionInput;
use chaos_parrot::RamaTransport;
use chaos_parrot::RequestTelemetry;
use chaos_parrot::build_conversation_headers;
use chaos_parrot::create_text_param_for_request;
use chaos_syslog::SessionTelemetry;
use http::HeaderMap as ApiHeaderMap;
use http::HeaderValue;
use tracing::warn;

use crate::api_bridge::map_api_error;
use crate::auth::ChaosAuth;
use crate::client_common::Prompt;
use crate::error::Result;
use crate::model_provider_info::ModelProviderInfo;
use crate::protocol::SubAgentSource;
use crate::tools::spec::create_tools_json_for_responses_api;

use super::{
    ApiTelemetry, AuthRequestTelemetryContext, CurrentClientSetup, ModelClient, ModelClientSession,
    ModelClientState, PendingUnauthorizedRetry, RESPONSES_COMPACT_ENDPOINT, RequestRouteTelemetry,
};

impl ModelClient {
    /// Creates a new session-scoped `ModelClient`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        auth_manager: Option<Arc<crate::AuthManager>>,
        conversation_id: chaos_ipc::ProcessId,
        provider: ModelProviderInfo,
        session_source: SessionSource,
        approval_policy: chaos_ipc::protocol::ApprovalPolicy,
        model_verbosity: Option<chaos_ipc::config_types::Verbosity>,
        enable_request_compression: bool,
        beta_features_header: Option<String>,
    ) -> Self {
        let representer = if provider.is_openai() {
            chaos_parrot::SessionRepresenter::openai()
        } else {
            chaos_parrot::SessionRepresenter::wannabe()
        };
        Self {
            state: Arc::new(ModelClientState {
                auth_manager,
                conversation_id,
                provider,
                session_source,
                approval_policy,
                model_verbosity,
                enable_request_compression,
                beta_features_header,
                resolved_wire: std::sync::OnceLock::new(),
                clamped: std::sync::atomic::AtomicBool::new(false),
                clamp_transport: tokio::sync::Mutex::new(None),
                clamp_mcp_bridge: tokio::sync::Mutex::new(None),
                session: std::sync::Mutex::new(Weak::new()),
                representer,
            }),
        }
    }

    pub(crate) fn bind_session(&self, session: &Arc<crate::chaos::Session>) {
        *self
            .state
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(session);
    }

    pub(super) async fn ensure_clamp_mcp_bridge(
        &self,
    ) -> std::result::Result<(PathBuf, String), String> {
        if let Some(existing) = self
            .state
            .clamp_mcp_bridge
            .lock()
            .await
            .as_ref()
            .map(|bridge| {
                (
                    bridge.socket_path().to_path_buf(),
                    bridge.token().to_string(),
                )
            })
        {
            return Ok(existing);
        }

        let session = self
            .state
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let bridge = crate::clamp_bridge::ClampSessionBridge::spawn(session)
            .await
            .map_err(|err| format!("failed to start clamp MCP bridge: {err}"))?;
        let output = (
            bridge.socket_path().to_path_buf(),
            bridge.token().to_string(),
        );
        let mut guard = self.state.clamp_mcp_bridge.lock().await;
        if guard.is_none() {
            *guard = Some(bridge);
        }
        Ok(output)
    }

    /// Creates a fresh turn-scoped streaming session.
    pub fn new_session(&self) -> ModelClientSession {
        ModelClientSession {
            client: self.clone(),
            turn_state: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Toggle clamped mode (Claude Code subprocess as transport).
    pub async fn set_clamped(&self, clamped: bool) {
        let was_clamped = self.state.clamped.swap(clamped, Ordering::Relaxed);
        if !clamped && was_clamped {
            let transport = {
                let mut guard = self.state.clamp_transport.lock().await;
                guard.take()
            };
            let bridge = {
                let mut guard = self.state.clamp_mcp_bridge.lock().await;
                guard.take()
            };
            if let Some(transport) = transport
                && let Err(err) = transport.shutdown().await
            {
                warn!("failed to shut down clamped transport: {err}");
            }
            if let Some(bridge) = bridge
                && let Err(err) = bridge.shutdown().await
            {
                warn!("failed to shut down clamp MCP bridge: {err}");
            }
        }
    }

    /// Whether the client is in clamped mode.
    pub fn is_clamped(&self) -> bool {
        self.state.clamped.load(Ordering::Relaxed)
    }

    /// Get info about the clamped Claude Code subprocess (if running).
    pub async fn clamp_info(&self) -> Option<chaos_clamp::ClampInfo> {
        let guard = self.state.clamp_transport.lock().await;
        guard.as_ref().and_then(chaos_clamp::ClampTransport::info)
    }

    /// Switch the model on the clamped Claude Code subprocess.
    pub async fn set_clamp_model(&self, model: &str) -> std::result::Result<(), String> {
        let mut guard = self.state.clamp_transport.lock().await;
        if let Some(transport) = guard.as_mut() {
            transport
                .set_model(model)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        } else {
            Err("clamp transport not running".to_string())
        }
    }

    /// Get the initialization response from the clamped subprocess.
    pub async fn clamp_init_response(&self) -> Option<serde_json::Value> {
        let guard = self.state.clamp_transport.lock().await;
        guard.as_ref().and_then(|t| t.init_response().cloned())
    }

    /// Compacts the current conversation history using the Compact endpoint.
    pub async fn compact_conversation_history(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        session_telemetry: &SessionTelemetry,
    ) -> Result<Vec<ResponseItem>> {
        if prompt.input.is_empty() {
            return Ok(Vec::new());
        }
        let client_setup = self.current_client_setup().await?;
        let transport = RamaTransport::default_client();
        let request_telemetry = Self::build_request_telemetry(
            session_telemetry,
            AuthRequestTelemetryContext::new(
                client_setup.auth.as_ref().map(ChaosAuth::auth_mode),
                &client_setup.api_auth,
                PendingUnauthorizedRetry::default(),
            ),
            RequestRouteTelemetry::for_endpoint(RESPONSES_COMPACT_ENDPOINT),
        );
        let client =
            ApiCompactClient::new(transport, client_setup.api_provider, client_setup.api_auth)
                .with_telemetry(Some(request_telemetry));

        let instructions = prompt.base_instructions.text.clone();
        let input = prompt.get_formatted_input();
        let mut tools = create_tools_json_for_responses_api(&prompt.tools)?;
        for tool_name in &model_info.native_server_side_tools {
            tools.push(serde_json::json!({"type": tool_name}));
        }
        let reasoning = Self::build_reasoning(model_info, effort, summary);
        let verbosity = if model_info.support_verbosity {
            self.state.model_verbosity.or(model_info.default_verbosity)
        } else {
            if self.state.model_verbosity.is_some() {
                warn!(
                    "model_verbosity is set but ignored as the model does not support verbosity: {}",
                    model_info.slug
                );
            }
            None
        };
        let text = create_text_param_for_request(verbosity, &prompt.output_schema);
        let payload = ApiCompactionInput {
            model: &model_info.slug,
            input: &input,
            instructions: &instructions,
            tools,
            parallel_tool_calls: prompt.parallel_tool_calls,
            reasoning,
            text,
        };

        let mut extra_headers = self.build_subagent_headers();
        extra_headers.extend(build_conversation_headers(Some(
            self.state.conversation_id.to_string(),
        )));
        client
            .compact_input(&payload, extra_headers)
            .await
            .map_err(map_api_error)
    }

    pub(super) fn build_subagent_headers(&self) -> ApiHeaderMap {
        let mut extra_headers = crate::default_client::default_headers();
        if let SessionSource::SubAgent(sub) = &self.state.session_source {
            let subagent = match sub {
                SubAgentSource::Review => "review".to_string(),
                SubAgentSource::Compact => "compact".to_string(),
                SubAgentSource::MemoryConsolidation => "memory_consolidation".to_string(),
                SubAgentSource::ProcessSpawn { .. } => "collab_spawn".to_string(),
                SubAgentSource::Other(label) => label.clone(),
            };
            if let Ok(val) = HeaderValue::from_str(&subagent) {
                extra_headers.insert("x-openai-subagent", val);
            }
        }
        extra_headers
    }

    pub(super) fn build_request_telemetry(
        session_telemetry: &SessionTelemetry,
        auth_context: AuthRequestTelemetryContext,
        request_route_telemetry: RequestRouteTelemetry,
    ) -> Arc<dyn RequestTelemetry> {
        let telemetry = Arc::new(ApiTelemetry::new(
            session_telemetry.clone(),
            auth_context,
            request_route_telemetry,
        ));
        let request_telemetry: Arc<dyn RequestTelemetry> = telemetry;
        request_telemetry
    }

    pub(super) fn build_reasoning(
        model_info: &ModelInfo,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
    ) -> Option<chaos_parrot::common::Reasoning> {
        if model_info.supports_reasoning_summaries {
            Some(chaos_parrot::common::Reasoning {
                effort: effort.or(model_info.default_reasoning_level),
                summary: if summary == ReasoningSummaryConfig::None {
                    None
                } else {
                    Some(summary)
                },
            })
        } else {
            None
        }
    }

    pub(super) async fn current_client_setup(&self) -> Result<CurrentClientSetup> {
        let auth = if self.state.provider.is_self_authenticated() {
            None
        } else {
            match self.state.auth_manager.as_ref() {
                Some(manager) => manager.auth().await,
                None => None,
            }
        };
        let api_provider = self
            .state
            .provider
            .to_api_provider(auth.as_ref().map(ChaosAuth::auth_mode))?;
        let api_auth =
            crate::api_bridge::auth_provider_from_auth(auth.clone(), &self.state.provider)?;
        Ok(CurrentClientSetup {
            auth,
            api_provider,
            api_auth,
        })
    }
}
