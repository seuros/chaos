//! Session- and turn-scoped helpers for talking to model provider APIs.
//!
//! `ModelClient` is intended to live for the lifetime of a Chaos session and holds the stable
//! configuration and state needed to talk to a provider (auth, provider selection, conversation id,
//! and feature-gated request behavior).
//!
//! Per-turn settings (model selection, reasoning controls, telemetry context, and turn metadata)
//! are passed explicitly to streaming and unary methods so that the turn lifetime is visible at the
//! call site.
//!
//! A [`ModelClientSession`] is created per turn and is used to stream one or more Responses API
//! requests during that turn. It caches a Responses WebSocket connection (opened lazily) and stores
//! per-turn state such as the `x-chaos-turn-state` token used for sticky routing.

pub(crate) mod state;
pub(crate) mod streaming;
pub(crate) mod tools;

pub(crate) use tools::active_clamp_turn_context;

#[cfg(test)]
pub(super) use tools::{
    ClampLocalToolKind, ClampToolRouting, clamp_permission_mode, clamp_tool_routing,
    render_clamp_full_prompt, render_latest_clamp_user_message,
};

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_ipc::config_types::ReasoningSummary as ReasoningSummaryConfig;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::config_types::Verbosity as VerbosityConfig;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::SessionSource;
use chaos_parrot::RequestTelemetry;
use chaos_parrot::ResponsesOptions as ApiResponsesOptions;
use chaos_parrot::SseTelemetry;
use chaos_parrot::TransportError;
use chaos_syslog::SessionTelemetry;
use http::StatusCode as HttpStatusCode;
use rama::error::BoxError;
use rama::http::sse::Event;

use crate::api_bridge::CoreAuthProvider;
use crate::auth::AuthMode;
use crate::auth::ChaosAuth;
use crate::model_provider_info::ModelProviderInfo;
use crate::model_provider_info::WireApi;
use crate::response_debug_context::extract_response_debug_context;
use crate::response_debug_context::telemetry_transport_error_message;
use crate::util::FeedbackRequestTags;
use crate::util::emit_feedback_request_tags;

// Wire-facing header names must remain "x-codex-*" so the ChatGPT codex
// proxy recognizes them and takes the modern routing path. Otherwise it
// falls back to a legacy path that injects `prompt_cache_retention` into
// the forwarded body, which upstream /v1/responses then rejects.
// TODO(parrot): move these wire-format constants into the parrot provider
// adapter — kern shouldn't know about ChatGPT-proxy-specific header names.
pub const X_CODEX_TURN_STATE_HEADER: &str = "x-codex-turn-state";
pub const X_CODEX_TURN_METADATA_HEADER: &str = "x-codex-turn-metadata";
pub const X_CODEX_BETA_FEATURES_HEADER: &str = "x-codex-beta-features";
pub const X_RESPONSESAPI_INCLUDE_TIMING_METRICS_HEADER: &str =
    "x-responsesapi-include-timing-metrics";
pub(super) const RESPONSES_ENDPOINT: &str = "/responses";
pub(super) const RESPONSES_COMPACT_ENDPOINT: &str = "/responses/compact";

// ── Private helper types used by streaming.rs and state.rs ───────────────────

/// Session-scoped state shared by all [`ModelClient`] clones.
#[derive(Debug)]
pub(super) struct ModelClientState {
    pub(super) auth_manager: Option<Arc<crate::AuthManager>>,
    pub(super) conversation_id: ProcessId,
    pub(super) provider: ModelProviderInfo,
    pub(super) session_source: SessionSource,
    pub(super) approval_policy: ApprovalPolicy,
    pub(super) model_verbosity: Option<VerbosityConfig>,
    pub(super) enable_request_compression: bool,
    pub(super) beta_features_header: Option<String>,
    /// Cached result of auto wire-format detection.
    pub(super) resolved_wire: OnceLock<WireApi>,
    /// When true, route all turns through the Claude Code subprocess (clamped mode).
    pub(super) clamped: AtomicBool,
    /// Persistent Claude Code subprocess for clamped mode.
    pub(super) clamp_transport: tokio::sync::Mutex<Option<chaos_clamp::ClampTransport>>,
    /// Session-bound MCP bridge for clamp subprocesses.
    pub(super) clamp_mcp_bridge:
        tokio::sync::Mutex<Option<crate::clamp_bridge::ClampSessionBridge>>,
    /// Back-reference to the owning session for clamp-side MCP routing.
    pub(super) session: StdMutex<Weak<crate::chaos::Session>>,
    /// Wire-format representer selected at session creation based on provider identity.
    pub(super) representer: chaos_parrot::SessionRepresenter,
}

/// Resolved API client setup for a single request attempt.
pub(super) struct CurrentClientSetup {
    pub(super) auth: Option<ChaosAuth>,
    pub(super) api_provider: chaos_parrot::Provider,
    pub(super) api_auth: CoreAuthProvider,
}

#[derive(Clone, Copy)]
pub(super) struct RequestRouteTelemetry {
    pub(super) endpoint: &'static str,
}

impl RequestRouteTelemetry {
    pub(super) fn for_endpoint(endpoint: &'static str) -> Self {
        Self { endpoint }
    }
}

pub(super) struct HttpTurnRequestConfig<'a> {
    pub(super) effort: Option<ReasoningEffortConfig>,
    pub(super) summary: ReasoningSummaryConfig,
    pub(super) service_tier: Option<ServiceTier>,
    pub(super) options: &'a ApiResponsesOptions,
}

// ── Telemetry / auth-retry types ─────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub(super) struct UnauthorizedRecoveryExecution {
    pub(super) mode: &'static str,
    pub(super) phase: &'static str,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PendingUnauthorizedRetry {
    pub(super) retry_after_unauthorized: bool,
    pub(super) recovery_mode: Option<&'static str>,
    pub(super) recovery_phase: Option<&'static str>,
}

impl PendingUnauthorizedRetry {
    pub(super) fn from_recovery(recovery: UnauthorizedRecoveryExecution) -> Self {
        Self {
            retry_after_unauthorized: true,
            recovery_mode: Some(recovery.mode),
            recovery_phase: Some(recovery.phase),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct AuthRequestTelemetryContext {
    pub(super) auth_mode: Option<&'static str>,
    pub(super) auth_header_attached: bool,
    pub(super) auth_header_name: Option<&'static str>,
    pub(super) retry_after_unauthorized: bool,
    pub(super) recovery_mode: Option<&'static str>,
    pub(super) recovery_phase: Option<&'static str>,
}

impl AuthRequestTelemetryContext {
    pub(super) fn new(
        auth_mode: Option<AuthMode>,
        api_auth: &CoreAuthProvider,
        retry: PendingUnauthorizedRetry,
    ) -> Self {
        Self {
            auth_mode: auth_mode.map(|mode| match mode {
                AuthMode::ApiKey => "ApiKey",
                AuthMode::Chatgpt => "Chatgpt",
            }),
            auth_header_attached: api_auth.auth_header_attached(),
            auth_header_name: api_auth.auth_header_name(),
            retry_after_unauthorized: retry.retry_after_unauthorized,
            recovery_mode: retry.recovery_mode,
            recovery_phase: retry.recovery_phase,
        }
    }
}

pub(super) struct ApiTelemetry {
    pub(super) session_telemetry: SessionTelemetry,
    pub(super) auth_context: AuthRequestTelemetryContext,
    pub(super) request_route_telemetry: RequestRouteTelemetry,
}

impl ApiTelemetry {
    pub(super) fn new(
        session_telemetry: SessionTelemetry,
        auth_context: AuthRequestTelemetryContext,
        request_route_telemetry: RequestRouteTelemetry,
    ) -> Self {
        Self {
            session_telemetry,
            auth_context,
            request_route_telemetry,
        }
    }
}

impl RequestTelemetry for ApiTelemetry {
    fn on_request(
        &self,
        attempt: u64,
        status: Option<HttpStatusCode>,
        error: Option<&TransportError>,
        duration: Duration,
    ) {
        let error_message = error.map(telemetry_transport_error_message);
        let status = status.map(|s| s.as_u16());
        let debug = error
            .map(extract_response_debug_context)
            .unwrap_or_default();
        self.session_telemetry.record_api_request(
            attempt,
            status,
            error_message.as_deref(),
            duration,
            self.auth_context.auth_header_attached,
            self.auth_context.auth_header_name,
            self.auth_context.retry_after_unauthorized,
            self.auth_context.recovery_mode,
            self.auth_context.recovery_phase,
            self.request_route_telemetry.endpoint,
            debug.request_id.as_deref(),
            debug.cf_ray.as_deref(),
            debug.auth_error.as_deref(),
            debug.auth_error_code.as_deref(),
        );
        emit_feedback_request_tags(&FeedbackRequestTags {
            endpoint: self.request_route_telemetry.endpoint,
            auth_header_attached: self.auth_context.auth_header_attached,
            auth_header_name: self.auth_context.auth_header_name,
            auth_mode: self.auth_context.auth_mode,
            auth_retry_after_unauthorized: Some(self.auth_context.retry_after_unauthorized),
            auth_recovery_mode: self.auth_context.recovery_mode,
            auth_recovery_phase: self.auth_context.recovery_phase,
            auth_connection_reused: None,
            auth_request_id: debug.request_id.as_deref(),
            auth_cf_ray: debug.cf_ray.as_deref(),
            auth_error: debug.auth_error.as_deref(),
            auth_error_code: debug.auth_error_code.as_deref(),
            auth_recovery_followup_success: self
                .auth_context
                .retry_after_unauthorized
                .then_some(error.is_none()),
            auth_recovery_followup_status: self
                .auth_context
                .retry_after_unauthorized
                .then_some(status)
                .flatten(),
        });
    }
}

impl SseTelemetry for ApiTelemetry {
    fn on_sse_poll(
        &self,
        result: &std::result::Result<
            Option<std::result::Result<Event, BoxError>>,
            tokio::time::error::Elapsed,
        >,
        duration: Duration,
    ) {
        self.session_telemetry.log_sse_event(result, duration);
    }
}

// ── Public structs ────────────────────────────────────────────────────────────

/// A session-scoped client for model-provider API calls.
///
/// This holds configuration and state that should be shared across turns within a Chaos session
/// (auth, provider selection, conversation id, feature-gated request behavior, and transport
/// fallback state).
///
/// Turn-scoped settings (model selection, reasoning controls, telemetry context, and turn
/// metadata) are passed explicitly to the relevant methods to keep turn lifetime visible at the
/// call site.
#[derive(Debug, Clone)]
pub struct ModelClient {
    pub(super) state: Arc<ModelClientState>,
}

/// A turn-scoped streaming session created from a [`ModelClient`].
///
/// Create a fresh `ModelClientSession` for each Chaos turn.
pub struct ModelClientSession {
    pub(super) client: ModelClient,
    /// Turn state for sticky routing.
    pub(super) turn_state: Arc<OnceLock<String>>,
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
