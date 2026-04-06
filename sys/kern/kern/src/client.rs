//! Session- and turn-scoped helpers for talking to model provider APIs.
//!
//! `ModelClient` is intended to live for the lifetime of a Codex session and holds the stable
//! configuration and state needed to talk to a provider (auth, provider selection, conversation id,
//! and feature-gated request behavior).
//!
//! Per-turn settings (model selection, reasoning controls, telemetry context, and turn metadata)
//! are passed explicitly to streaming and unary methods so that the turn lifetime is visible at the
//! call site.
//!
//! A [`ModelClientSession`] is created per turn and is used to stream one or more Responses API
//! requests during that turn. It caches a Responses WebSocket connection (opened lazily) and stores
//! per-turn state such as the `x-codex-turn-state` token used for sticky routing.
//!
//! WebSocket prewarm is a v2-only `response.create` with `generate=false`; it waits for completion
//! so the next request can reuse the same connection and `previous_response_id`.
//!
//! Turn execution performs prewarm as a best-effort step before the first stream request so the
//! subsequent request can reuse the same connection.
//!
//! ## Retry-Budget Tradeoff
//!
//! WebSocket prewarm is treated as the first websocket connection attempt for a turn. If it
//! fails, normal stream retry/fallback logic handles recovery on the same turn.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::api_bridge::CoreAuthProvider;
use crate::api_bridge::abi_error_to_api_error;
use crate::api_bridge::auth_provider_from_auth;
use crate::api_bridge::map_api_error;
use crate::auth::UnauthorizedRecovery;
use chaos_parrot::CompactClient as ApiCompactClient;
use chaos_parrot::CompactionInput as ApiCompactionInput;
use chaos_parrot::RamaTransport;
use chaos_parrot::RequestTelemetry;
use chaos_parrot::ResponsesOptions as ApiResponsesOptions;
use chaos_parrot::SseTelemetry;
use chaos_parrot::TransportError;
use chaos_parrot::anthropic::AnthropicAdapter;
use chaos_parrot::anthropic::AnthropicAuth;
use chaos_parrot::build_conversation_headers;
use chaos_parrot::common::Reasoning;
use chaos_parrot::create_text_param_for_request;
use chaos_parrot::error::ApiError;
use chaos_parrot::openai::OpenAiAdapter;
use chaos_parrot::requests::responses::Compression;
use chaos_syslog::SessionTelemetry;

use chaos_abi::AbiError;
use chaos_abi::FreeformToolDef;
use chaos_abi::FunctionToolDef;
use chaos_abi::ModelAdapter;
use chaos_abi::ReasoningConfig as AbiReasoningConfig;
use chaos_abi::ToolDef as AbiToolDef;
use chaos_abi::TurnRequest as AbiTurnRequest;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::ReasoningSummary as ReasoningSummaryConfig;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::config_types::Verbosity as VerbosityConfig;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::FileSystemPermissions;
use chaos_ipc::models::PermissionProfile;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::permissions::FileSystemSandboxPolicy;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::WarningEvent;
use chaos_ipc::request_permissions::RequestPermissionProfile;
use chaos_ipc::request_permissions::RequestPermissionsArgs;
use futures::StreamExt;
use http::HeaderMap as ApiHeaderMap;
use http::HeaderValue;
use http::StatusCode as HttpStatusCode;
use http::StatusCode;
use rama::error::BoxError;
use rama::http::sse::Event;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::instrument;
use tracing::warn;

use crate::AuthManager;
use crate::auth::AuthMode;
use crate::auth::ChaosAuth;
use crate::auth::RefreshTokenError;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::client_common::ResponseStream;
use crate::client_common::tools::ToolSpec;
use crate::exec_policy::ExecApprovalRequest;

use crate::error::ChaosErr;
use crate::error::Result;
use crate::flags::CODEX_RS_SSE_FIXTURE;
use crate::model_provider_info::ModelProviderInfo;
use crate::model_provider_info::WireApi;
use chaos_parrot::chat_completions::ChatCompletionsAdapter;

use crate::response_debug_context::extract_response_debug_context;
use crate::response_debug_context::telemetry_transport_error_message;
use crate::tools::spec::create_tools_json_for_responses_api;
use crate::util::FeedbackRequestTags;
use crate::util::emit_feedback_auth_recovery_tags;
use crate::util::emit_feedback_request_tags;
use serde_json::Value;
use serde_json::json;

pub const X_CODEX_TURN_STATE_HEADER: &str = "x-codex-turn-state";
pub const X_CODEX_TURN_METADATA_HEADER: &str = "x-codex-turn-metadata";
pub const X_RESPONSESAPI_INCLUDE_TIMING_METRICS_HEADER: &str =
    "x-responsesapi-include-timing-metrics";
const RESPONSES_ENDPOINT: &str = "/responses";
const RESPONSES_COMPACT_ENDPOINT: &str = "/responses/compact";

/// Session-scoped state shared by all [`ModelClient`] clones.
///
/// This is intentionally kept minimal so `ModelClient` does not need to hold a full `Config`. Most
/// configuration is per turn and is passed explicitly to streaming/unary methods.
#[derive(Debug)]
struct ModelClientState {
    auth_manager: Option<Arc<AuthManager>>,
    conversation_id: ProcessId,
    provider: ModelProviderInfo,
    session_source: SessionSource,
    approval_policy: ApprovalPolicy,
    model_verbosity: Option<VerbosityConfig>,
    enable_request_compression: bool,
    beta_features_header: Option<String>,
    /// Cached result of auto wire-format detection. Set on the first successful
    /// `Auto` probe; subsequent turns reuse the winner without re-probing.
    resolved_wire: OnceLock<WireApi>,
    /// When true, route all turns through the Claude Code subprocess (clamped mode).
    clamped: AtomicBool,
    /// Persistent Claude Code subprocess for clamped mode.
    clamp_transport: tokio::sync::Mutex<Option<chaos_clamp::ClampTransport>>,
    /// Session-bound MCP bridge for clamp subprocesses.
    clamp_mcp_bridge: tokio::sync::Mutex<Option<crate::clamp_bridge::ClampSessionBridge>>,
    /// Back-reference to the owning session for clamp-side MCP routing.
    session: StdMutex<Weak<crate::chaos::Session>>,
}

/// Resolved API client setup for a single request attempt.
///
/// Keeping this as a single bundle ensures prewarm and normal request paths
/// share the same auth/provider setup flow.
struct CurrentClientSetup {
    auth: Option<ChaosAuth>,
    api_provider: chaos_parrot::Provider,
    api_auth: CoreAuthProvider,
}

#[derive(Clone, Copy)]
struct RequestRouteTelemetry {
    endpoint: &'static str,
}

impl RequestRouteTelemetry {
    fn for_endpoint(endpoint: &'static str) -> Self {
        Self { endpoint }
    }
}

/// A session-scoped client for model-provider API calls.
///
/// This holds configuration and state that should be shared across turns within a Codex session
/// (auth, provider selection, conversation id, feature-gated request behavior, and transport
/// fallback state).
///
/// WebSocket fallback is session-scoped: once a turn activates the HTTP fallback, subsequent turns
/// will also use HTTP for the remainder of the session.
///
/// Turn-scoped settings (model selection, reasoning controls, telemetry context, and turn
/// metadata) are passed explicitly to the relevant methods to keep turn lifetime visible at the
/// call site.
#[derive(Debug, Clone)]
pub struct ModelClient {
    state: Arc<ModelClientState>,
}

/// A turn-scoped streaming session created from a [`ModelClient`].
///
/// The session establishes a Responses WebSocket connection lazily and reuses it across multiple
/// requests within the turn. It also caches per-turn state:
///
/// - The last full request, so subsequent calls can reuse incremental websocket request payloads
///   only when the current request is an incremental extension of the previous one.
/// - The `x-codex-turn-state` sticky-routing token, which must be replayed for all requests within
///   the same turn.
///
/// Create a fresh `ModelClientSession` for each Codex turn. Reusing it across turns would replay
/// the previous turn's sticky-routing token into the next turn, which violates the client/server
/// contract and can cause routing bugs.
pub struct ModelClientSession {
    client: ModelClient,
    /// Turn state for sticky routing.
    ///
    /// This is an `OnceLock` that stores the turn state value received from the server
    /// on turn start via the `x-codex-turn-state` response header. Once set, this value
    /// should be sent back to the server in the `x-codex-turn-state` request header for
    /// all subsequent requests within the same turn to maintain sticky routing.
    ///
    /// This is a contract between the client and server: we receive it at turn start,
    /// keep sending it unchanged between turn requests (e.g., for retries, incremental
    /// appends, or continuation requests), and must not send it between different turns.
    turn_state: Arc<OnceLock<String>>,
}

struct HttpTurnRequestConfig<'a> {
    effort: Option<ReasoningEffortConfig>,
    summary: ReasoningSummaryConfig,
    service_tier: Option<ServiceTier>,
    options: &'a ApiResponsesOptions,
}

impl ModelClient {
    /// Creates a new session-scoped `ModelClient`.
    ///
    /// All arguments are expected to be stable for the lifetime of a Codex session. Per-turn values
    /// are passed to [`ModelClientSession::stream`] (and other turn-scoped methods) explicitly.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        auth_manager: Option<Arc<AuthManager>>,
        conversation_id: ProcessId,
        provider: ModelProviderInfo,
        session_source: SessionSource,
        approval_policy: ApprovalPolicy,
        model_verbosity: Option<VerbosityConfig>,
        enable_request_compression: bool,
        beta_features_header: Option<String>,
    ) -> Self {
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
                resolved_wire: OnceLock::new(),
                clamped: AtomicBool::new(false),
                clamp_transport: tokio::sync::Mutex::new(None),
                clamp_mcp_bridge: tokio::sync::Mutex::new(None),
                session: StdMutex::new(Weak::new()),
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

    async fn ensure_clamp_mcp_bridge(&self) -> std::result::Result<(PathBuf, String), String> {
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
            turn_state: Arc::new(OnceLock::new()),
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

    /// Get the initialization response from the clamped subprocess (models, commands, etc.).
    pub async fn clamp_init_response(&self) -> Option<serde_json::Value> {
        let guard = self.state.clamp_transport.lock().await;
        guard.as_ref().and_then(|t| t.init_response().cloned())
    }

    /// Compacts the current conversation history using the Compact endpoint.
    ///
    /// This is a unary call (no streaming) that returns a new list of
    /// `ResponseItem`s representing the compacted transcript.
    ///
    /// The model selection and telemetry context are passed explicitly to keep `ModelClient`
    /// session-scoped.
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
        let tools = create_tools_json_for_responses_api(&prompt.tools)?;
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

    fn build_subagent_headers(&self) -> ApiHeaderMap {
        let mut extra_headers = crate::default_client::default_headers();
        if let SessionSource::SubAgent(sub) = &self.state.session_source {
            let subagent = match sub {
                crate::protocol::SubAgentSource::Review => "review".to_string(),
                crate::protocol::SubAgentSource::Compact => "compact".to_string(),
                crate::protocol::SubAgentSource::MemoryConsolidation => {
                    "memory_consolidation".to_string()
                }
                crate::protocol::SubAgentSource::ProcessSpawn { .. } => "collab_spawn".to_string(),
                crate::protocol::SubAgentSource::Other(label) => label.clone(),
            };
            if let Ok(val) = HeaderValue::from_str(&subagent) {
                extra_headers.insert("x-openai-subagent", val);
            }
        }
        extra_headers
    }

    /// Builds request telemetry for unary API calls (e.g., Compact endpoint).
    fn build_request_telemetry(
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

    fn build_reasoning(
        model_info: &ModelInfo,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
    ) -> Option<Reasoning> {
        if model_info.supports_reasoning_summaries {
            Some(Reasoning {
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

    /// Returns auth + provider configuration resolved from the current session auth state.
    ///
    /// This centralizes setup used by both prewarm and normal request paths so they stay in
    /// lockstep when auth/provider resolution changes.
    async fn current_client_setup(&self) -> Result<CurrentClientSetup> {
        let auth = match self.state.auth_manager.as_ref() {
            Some(manager) => manager.auth().await,
            None => None,
        };
        let api_provider = self
            .state
            .provider
            .to_api_provider(auth.as_ref().map(ChaosAuth::auth_mode))?;
        let api_auth = auth_provider_from_auth(auth.clone(), &self.state.provider)?;
        Ok(CurrentClientSetup {
            auth,
            api_provider,
            api_auth,
        })
    }

}

fn clamp_permission_mode(approval_policy: ApprovalPolicy) -> String {
    match approval_policy {
        ApprovalPolicy::Headless => "bypassPermissions",
        ApprovalPolicy::Supervised | ApprovalPolicy::Interactive | ApprovalPolicy::Granular(_) => {
            "default"
        }
    }
    .to_string()
}

fn build_clamp_mcp_config(socket_path: &Path, token: &str) -> Value {
    let command = std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| "chaos".to_string());
    serde_json::json!({
        "mcpServers": {
            "chaos": {
                "command": command,
                "args": ["clamp-session-bridge"],
                "env": {
                    "CHAOS_CLAMP_MCP_SOCKET": socket_path.to_string_lossy(),
                    "CHAOS_CLAMP_MCP_TOKEN": token
                }
            }
        }
    })
}

pub(crate) const CLAMP_NATIVE_PASSTHROUGH_TOOLS: &[&str] = &["WebSearch", "WebFetch"];
const CLAMP_LOCAL_BUILTIN_TOOLS: &[&str] = &[
    "Bash",
    "Read",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookRead",
    "NotebookEdit",
    "Glob",
    "Grep",
    "LS",
];
const CLAMP_UNSUPPORTED_BUILTIN_TOOLS: &[&str] = &["Task", "TodoRead", "TodoWrite"];

pub(crate) fn build_clamp_disallowed_tools() -> Vec<String> {
    CLAMP_LOCAL_BUILTIN_TOOLS
        .iter()
        .chain(CLAMP_UNSUPPORTED_BUILTIN_TOOLS.iter())
        .map(|tool| (*tool).to_string())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClampToolPermissionDecision {
    Allow,
    AskPermissions {
        permissions: PermissionProfile,
        reason: String,
    },
    AskCommandApproval {
        command: Vec<String>,
        reason: String,
    },
    Deny(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClampLocalToolKind {
    Shell,
    FsRead,
    FsWrite,
    FsReadPathOptional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClampToolRouting {
    Passthrough,
    Local {
        local_tool_name: &'static str,
        kind: ClampLocalToolKind,
    },
}

fn clamp_tool_routing(tool_name: &str) -> Option<ClampToolRouting> {
    if CLAMP_NATIVE_PASSTHROUGH_TOOLS.contains(&tool_name) {
        return Some(ClampToolRouting::Passthrough);
    }

    match tool_name {
        // Route the rest through our local registry categories.
        "Bash" => Some(ClampToolRouting::Local {
            local_tool_name: "exec_command",
            kind: ClampLocalToolKind::Shell,
        }),
        "Read" => Some(ClampToolRouting::Local {
            local_tool_name: "read_file",
            kind: ClampLocalToolKind::FsRead,
        }),
        "NotebookRead" => Some(ClampToolRouting::Local {
            local_tool_name: "read_file",
            kind: ClampLocalToolKind::FsRead,
        }),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => Some(ClampToolRouting::Local {
            local_tool_name: "apply_patch",
            kind: ClampLocalToolKind::FsWrite,
        }),
        "Glob" | "Grep" | "LS" => Some(ClampToolRouting::Local {
            local_tool_name: "read_file",
            kind: ClampLocalToolKind::FsReadPathOptional,
        }),
        _ => None,
    }
}

fn clamp_permission_allow_response(input: Value) -> Value {
    serde_json::json!({
        "behavior": "allow",
        "updatedInput": input
    })
}

fn clamp_permission_deny_response(message: impl Into<String>) -> Value {
    serde_json::json!({
        "behavior": "deny",
        "message": message.into()
    })
}

fn clamp_resolve_input_path(
    input: &Value,
    cwd: &std::path::Path,
    keys: &[&str],
) -> Option<chaos_realpath::AbsolutePathBuf> {
    let object = input.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(|value| value.as_str())
        .and_then(|path| chaos_realpath::AbsolutePathBuf::resolve_path_against_base(path, cwd).ok())
}

fn clamp_read_permission(path: chaos_realpath::AbsolutePathBuf) -> PermissionProfile {
    PermissionProfile {
        network: None,
        file_system: Some(FileSystemPermissions {
            read: Some(vec![path]),
            write: None,
        }),
        macos: None,
    }
}

fn clamp_write_permission(path: chaos_realpath::AbsolutePathBuf) -> PermissionProfile {
    PermissionProfile {
        network: None,
        file_system: Some(FileSystemPermissions {
            read: None,
            write: Some(vec![path]),
        }),
        macos: None,
    }
}

fn clamp_effective_file_system_policy(
    turn: &crate::chaos::TurnContext,
    granted_permissions: Option<&PermissionProfile>,
) -> FileSystemSandboxPolicy {
    crate::sandboxing::effective_file_system_sandbox_policy(
        &turn.file_system_sandbox_policy,
        granted_permissions,
    )
}

fn clamp_tool_permission_decision(
    tool_name: &str,
    input: &Value,
    cwd: &std::path::Path,
    file_system_policy: &FileSystemSandboxPolicy,
) -> ClampToolPermissionDecision {
    let Some(routing) = clamp_tool_routing(tool_name) else {
        return ClampToolPermissionDecision::Deny(format!(
            "Claude Code built-in tool '{tool_name}' is not supported in clamp mode; use Chaos-managed tools instead."
        ));
    };

    match routing {
        ClampToolRouting::Passthrough => ClampToolPermissionDecision::Allow,
        ClampToolRouting::Local {
            local_tool_name,
            kind: ClampLocalToolKind::Shell,
        } => {
            let command = input
                .get("command")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            match command {
                Some(command) => ClampToolPermissionDecision::AskCommandApproval {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        command.to_string(),
                    ],
                    reason: format!(
                        "Claude Code {tool_name} routes through local tool '{local_tool_name}' and requests permission to run a shell command."
                    ),
                },
                None => ClampToolPermissionDecision::Deny(format!(
                    "Claude Code {tool_name} request is missing a command."
                )),
            }
        }
        ClampToolRouting::Local {
            local_tool_name,
            kind: ClampLocalToolKind::FsRead,
        } => match clamp_resolve_input_path(input, cwd, &["file_path", "path"]) {
            Some(path) if file_system_policy.can_read_path_with_cwd(path.as_path(), cwd) => {
                ClampToolPermissionDecision::Allow
            }
            Some(path) => ClampToolPermissionDecision::AskPermissions {
                permissions: clamp_read_permission(path),
                reason: format!(
                    "Claude Code {tool_name} routes through local tool '{local_tool_name}' and requests filesystem read access."
                ),
            },
            None => ClampToolPermissionDecision::Deny(format!(
                "Claude Code {tool_name} request is missing a readable path."
            )),
        },
        ClampToolRouting::Local {
            local_tool_name,
            kind: ClampLocalToolKind::FsWrite,
        } => match clamp_resolve_input_path(input, cwd, &["file_path", "path"]) {
            Some(path) if file_system_policy.can_write_path_with_cwd(path.as_path(), cwd) => {
                ClampToolPermissionDecision::Allow
            }
            Some(path) => ClampToolPermissionDecision::AskPermissions {
                permissions: clamp_write_permission(path),
                reason: format!(
                    "Claude Code {tool_name} routes through local tool '{local_tool_name}' and requests filesystem write access."
                ),
            },
            None => ClampToolPermissionDecision::Deny(format!(
                "Claude Code {tool_name} request is missing a writable path."
            )),
        },
        ClampToolRouting::Local {
            local_tool_name,
            kind: ClampLocalToolKind::FsReadPathOptional,
        } => match clamp_resolve_input_path(input, cwd, &["path"]) {
            Some(path) if file_system_policy.can_read_path_with_cwd(path.as_path(), cwd) => {
                ClampToolPermissionDecision::Allow
            }
            Some(path) => ClampToolPermissionDecision::AskPermissions {
                permissions: clamp_read_permission(path),
                reason: format!(
                    "Claude Code {tool_name} routes through local tool '{local_tool_name}' and requests filesystem read access."
                ),
            },
            None => ClampToolPermissionDecision::Allow,
        },
    }
}

pub(crate) async fn active_clamp_turn_context(
    session: &crate::chaos::Session,
) -> Option<Arc<crate::chaos::TurnContext>> {
    let active = session.active_turn.lock().await;
    let (_, task) = active.as_ref()?.tasks.first()?;
    Some(Arc::clone(&task.turn_context))
}

async fn handle_clamp_mcp_message(
    session: Weak<crate::chaos::Session>,
    server_name: String,
    message: Value,
) -> std::result::Result<Value, String> {
    let Some(session) = session.upgrade() else {
        return Err("session closed".to_string());
    };

    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message
        .get("method")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing MCP method".to_string())?;

    match method {
        "tools/call" => {
            let params = message
                .get("params")
                .and_then(|v| v.as_object())
                .ok_or_else(|| "missing MCP params".to_string())?;
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing MCP tool name".to_string())?
                .to_string();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let raw_arguments = serde_json::to_string(&arguments)
                .map_err(|err| format!("failed to serialize MCP arguments: {err}"))?;
            let turn_context = active_clamp_turn_context(&session)
                .await
                .ok_or_else(|| "no active turn for clamp MCP tool call".to_string())?;
            let call_id = format!("clamp_mcp_{}", uuid::Uuid::now_v7());
            let result = crate::mcp_tool_call::handle_mcp_tool_call(
                Arc::clone(&session),
                &turn_context,
                call_id,
                server_name,
                tool_name,
                raw_arguments,
            )
            .await;
            let result_value = serde_json::to_value(&result)
                .map_err(|err| format!("failed to serialize MCP result: {err}"))?;
            Ok(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result_value
            }))
        }
        _ => Ok(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("unsupported clamp MCP method: {method}")
            }
        })),
    }
}

async fn handle_clamp_tool_permission(
    session: Weak<crate::chaos::Session>,
    tool_name: String,
    input: Value,
    tool_use_id: Option<String>,
) -> std::result::Result<Value, String> {
    let Some(session) = session.upgrade() else {
        return Err("session closed".to_string());
    };
    let turn_context = active_clamp_turn_context(&session)
        .await
        .ok_or_else(|| "no active turn for clamp tool permission".to_string())?;
    let granted_permissions = crate::sandboxing::merge_permission_profiles(
        session.granted_session_permissions().await.as_ref(),
        session.granted_turn_permissions().await.as_ref(),
    );
    let file_system_policy =
        clamp_effective_file_system_policy(&turn_context, granted_permissions.as_ref());
    let decision = clamp_tool_permission_decision(
        &tool_name,
        &input,
        turn_context.cwd.as_path(),
        &file_system_policy,
    );
    let call_id = tool_use_id.unwrap_or_else(|| format!("clamp_tool_{}", uuid::Uuid::now_v7()));

    match decision {
        ClampToolPermissionDecision::Allow => Ok(clamp_permission_allow_response(input)),
        ClampToolPermissionDecision::Deny(message) => Ok(clamp_permission_deny_response(message)),
        ClampToolPermissionDecision::AskPermissions {
            permissions,
            reason,
        } => {
            let response = session
                .request_permissions(
                    turn_context.as_ref(),
                    call_id,
                    RequestPermissionsArgs {
                        reason: Some(reason.clone()),
                        permissions: RequestPermissionProfile::from(permissions.clone()),
                    },
                )
                .await
                .ok_or_else(|| "clamp permission request cancelled".to_string())?;
            let granted = crate::sandboxing::intersect_permission_profiles(
                permissions.clone(),
                response.permissions.into(),
            );
            if granted == permissions {
                Ok(clamp_permission_allow_response(input))
            } else {
                Ok(clamp_permission_deny_response(format!(
                    "{reason} Access was not granted."
                )))
            }
        }
        ClampToolPermissionDecision::AskCommandApproval { command, reason } => {
            let exec_approval_requirement = session
                .services
                .exec_policy
                .create_exec_approval_requirement_for_command(ExecApprovalRequest {
                    command: &command,
                    approval_policy: turn_context.approval_policy.value(),
                    sandbox_policy: turn_context.sandbox_policy.get(),
                    file_system_sandbox_policy: &turn_context.file_system_sandbox_policy,
                    sandbox_permissions: chaos_ipc::models::SandboxPermissions::UseDefault,
                    prefix_rule: None,
                })
                .await;
            match exec_approval_requirement {
                crate::tools::sandboxing::ExecApprovalRequirement::Skip { .. } => {
                    Ok(clamp_permission_allow_response(input))
                }
                crate::tools::sandboxing::ExecApprovalRequirement::Forbidden { reason } => {
                    Ok(clamp_permission_deny_response(reason))
                }
                crate::tools::sandboxing::ExecApprovalRequirement::NeedsApproval {
                    reason: approval_reason,
                    proposed_execpolicy_amendment,
                } => {
                    let review_decision = session
                        .request_command_approval(
                            turn_context.as_ref(),
                            call_id,
                            None,
                            command,
                            turn_context.cwd.clone(),
                            approval_reason.or(Some(reason)),
                            None,
                            proposed_execpolicy_amendment,
                            None,
                            None,
                            None,
                        )
                        .await;
                    if matches!(
                        review_decision,
                        chaos_ipc::protocol::ReviewDecision::Approved
                            | chaos_ipc::protocol::ReviewDecision::ApprovedForSession
                    ) {
                        Ok(clamp_permission_allow_response(input))
                    } else {
                        Ok(clamp_permission_deny_response(
                            "Command execution was not approved.",
                        ))
                    }
                }
            }
        }
    }
}

async fn handle_clamp_hook_callback(
    session: Weak<crate::chaos::Session>,
    callback_id: String,
    _input: Value,
    tool_use_id: Option<String>,
) -> std::result::Result<Value, String> {
    let Some(session) = session.upgrade() else {
        return Err("session closed".to_string());
    };
    if let Some(turn_context) = active_clamp_turn_context(&session).await {
        session
            .send_event(
                turn_context.as_ref(),
                EventMsg::Warning(WarningEvent {
                    message: format!(
                        "Clamp received unexpected Claude hook callback '{}'{}; clamp sessions do not currently register callback hooks.",
                        callback_id,
                        tool_use_id
                            .as_deref()
                            .map(|id| format!(" (tool_use_id: {id})"))
                            .unwrap_or_default()
                    ),
                }),
            )
            .await;
    }
    Ok(serde_json::json!({}))
}

fn render_clamp_content_items(content: &[ContentItem]) -> String {
    content
        .iter()
        .map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => text.clone(),
            ContentItem::InputImage { image_url } => {
                if image_url.starts_with("data:") {
                    "[image: inline data omitted]".to_string()
                } else {
                    format!("[image: {image_url}]")
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_json_pretty<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|err| format!("<serialization error: {err}>"))
}

fn clamp_elide_large_text(text: &str) -> String {
    const MAX_CHARS: usize = 8_000;
    let mut chars = text.chars();
    let preview: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!(
            "{preview}\n...[truncated {} chars]",
            text.chars().count() - MAX_CHARS
        )
    } else {
        preview
    }
}

fn render_clamp_response_item(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::Message { role, content, .. } => Some(format!(
            "<message role=\"{role}\">\n{}\n</message>",
            render_clamp_content_items(content)
        )),
        ResponseItem::Reasoning { summary, .. } => {
            let text = summary
                .iter()
                .map(|entry| match entry {
                    chaos_ipc::models::ReasoningItemReasoningSummary::SummaryText { text } => {
                        text.as_str()
                    }
                })
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then(|| format!("<reasoning_summary>\n{text}\n</reasoning_summary>"))
        }
        ResponseItem::LocalShellCall {
            call_id,
            status,
            action,
            ..
        } => Some(format!(
            "<local_shell_call call_id=\"{}\" status=\"{}\">\n{}\n</local_shell_call>",
            call_id.as_deref().unwrap_or(""),
            serde_json::to_string(status).unwrap_or_else(|_| "\"unknown\"".to_string()),
            render_json_pretty(action)
        )),
        ResponseItem::FunctionCall {
            name,
            call_id,
            arguments,
            namespace,
            ..
        } => Some(format!(
            "<function_call name=\"{name}\" namespace=\"{}\" call_id=\"{call_id}\">\n{}\n</function_call>",
            namespace.as_deref().unwrap_or(""),
            arguments
        )),
        ResponseItem::ToolSearchCall {
            call_id,
            status,
            execution,
            arguments,
            ..
        } => Some(format!(
            "<tool_search_call call_id=\"{}\" status=\"{}\" execution=\"{execution}\">\n{}\n</tool_search_call>",
            call_id.as_deref().unwrap_or(""),
            status.as_deref().unwrap_or(""),
            render_json_pretty(arguments)
        )),
        ResponseItem::FunctionCallOutput { call_id, output }
        | ResponseItem::CustomToolCallOutput { call_id, output } => Some(format!(
            "<tool_output call_id=\"{call_id}\">\n{}\n</tool_output>",
            clamp_elide_large_text(
                &output
                    .body
                    .to_text()
                    .unwrap_or_else(|| render_json_pretty(output))
            )
        )),
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            status,
            ..
        } => Some(format!(
            "<custom_tool_call name=\"{name}\" call_id=\"{call_id}\" status=\"{}\">\n{input}\n</custom_tool_call>",
            status.as_deref().unwrap_or("")
        )),
        ResponseItem::ToolSearchOutput {
            call_id,
            status,
            execution,
            tools,
        } => Some(format!(
            "<tool_search_output call_id=\"{}\" status=\"{status}\" execution=\"{execution}\">\n{}\n</tool_search_output>",
            call_id.as_deref().unwrap_or(""),
            render_json_pretty(tools)
        )),
        ResponseItem::WebSearchCall { status, action, .. } => Some(format!(
            "<web_search_call status=\"{}\">\n{}\n</web_search_call>",
            status.as_deref().unwrap_or(""),
            action.as_ref().map(render_json_pretty).unwrap_or_default()
        )),
        ResponseItem::ImageGenerationCall {
            status,
            revised_prompt,
            result,
            ..
        } => Some(format!(
            "<image_generation_call status=\"{status}\">\nrevised_prompt: {}\nresult: {}\n</image_generation_call>",
            revised_prompt.as_deref().unwrap_or(""),
            clamp_elide_large_text(result)
        )),
        ResponseItem::GhostSnapshot { .. } => {
            Some("<ghost_snapshot>[omitted]</ghost_snapshot>".to_string())
        }
        ResponseItem::Compaction { .. } => Some("<compaction>[omitted]</compaction>".to_string()),
        ResponseItem::Other => Some("<other_response_item />".to_string()),
    }
}

fn render_clamp_full_prompt(prompt: &Prompt) -> String {
    let rendered_items = prompt
        .get_formatted_input()
        .iter()
        .filter_map(render_clamp_response_item)
        .collect::<Vec<_>>();

    if rendered_items.is_empty() {
        return "Chaos restored an empty conversation state. Respond to the latest user request."
            .to_string();
    }

    format!(
        "Chaos restored the current Codex conversation state after connecting Claude Code.\n\
Treat the transcript below as authoritative prior context, including tool calls and tool outputs that already happened.\n\
Continue from the latest user request instead of restarting the conversation.\n\n\
<conversation_state>\n{}\n</conversation_state>",
        rendered_items.join("\n\n")
    )
}

fn render_latest_clamp_user_message(prompt: &Prompt) -> String {
    prompt
        .get_formatted_input()
        .iter()
        .rev()
        .find_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                let rendered = render_clamp_content_items(content);
                (!rendered.is_empty()).then_some(rendered)
            }
            _ => None,
        })
        .unwrap_or_else(|| render_clamp_full_prompt(prompt))
}

impl ModelClientSession {
    fn build_http_turn_request(
        &self,
        provider: &chaos_parrot::Provider,
        prompt: &Prompt,
        model_info: &ModelInfo,
        config: HttpTurnRequestConfig<'_>,
    ) -> Result<AbiTurnRequest> {
        let input = prompt.get_formatted_input();
        let openai_tools = create_tools_json_for_responses_api(&prompt.tools)?;
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
    /// Builds shared Responses API transport options and request-body options.
    ///
    /// Keeping option construction in one place ensures request-scoped headers are consistent
    /// regardless of transport choice.
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

    /// Streams a turn via the OpenAI Responses API.
    ///
    /// Handles SSE fixtures, reasoning summaries, verbosity, and the
    /// `text` controls used for output schemas.
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
        if let Some(path) = &*CODEX_RS_SSE_FIXTURE {
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
            .map(super::auth::AuthManager::unauthorized_recovery);
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

    /// Streams a turn via a clamped Claude Code subprocess.
    ///
    /// The Claude Code CLI is driven as a headless subprocess using the
    /// stream-json control protocol. The user's MAX subscription provides
    /// the LLM tokens; Chaos provides tools, UI, and orchestration.
    #[instrument(
        name = "model_client.stream_clamped",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
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

        // Get or create the persistent transport.
        // The transport lives on ModelClientState and persists across turns,
        // so Claude Code keeps conversation context.
        let clamp_state = Arc::clone(&self.client.state);

        let (tx_event, rx_event) =
            mpsc::channel::<std::result::Result<ResponseEvent, ApiError>>(256);

        let session_telemetry = session_telemetry.clone();
        tokio::spawn(async move {
            let mut guard = clamp_state.clamp_transport.lock().await;
            let mut spawned_fresh = false;
            let session = clamp_state
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();

            // Spawn + initialize on first use; reuse on subsequent turns.
            if guard.is_none() {
                let permission_session = session.clone();
                let hook_session = session.clone();
                let mcp_session = session.clone();
                let (bridge_socket_path, bridge_token) =
                    match client.ensure_clamp_mcp_bridge().await {
                        Ok(bridge) => bridge,
                        Err(err) => {
                            let _ = tx_event.send(Err(ApiError::Stream(err))).await;
                            return;
                        }
                    };
                let config = ClampConfig {
                    bare_mode: true,
                    system_prompt: Some(system_prompt),
                    permission_mode: Some(clamp_permission_mode(clamp_state.approval_policy)),
                    mcp_config: Some(build_clamp_mcp_config(&bridge_socket_path, &bridge_token)),
                    disallowed_tools: build_clamp_disallowed_tools(),
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
                                .send(Err(ApiError::Stream(format!("clamp init failed: {e}"))))
                                .await;
                            return;
                        }
                        // Cache the models list for the TUI model picker.
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
                            .send(Err(ApiError::Stream(format!("clamp spawn failed: {e}"))))
                            .await;
                        return;
                    }
                }
            }

            let Some(transport) = guard.as_mut() else {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "clamp transport missing after initialization".to_string(),
                    )))
                    .await;
                return;
            };

            if let Err(e) = transport.set_model(&clamp_model_slug).await {
                *guard = None;
                let _ = tx_event
                    .send(Err(ApiError::Stream(format!(
                        "clamp set_model failed: {e}"
                    ))))
                    .await;
                return;
            }

            let _ = tx_event.send(Ok(ResponseEvent::Created)).await;

            // Kern expects an OutputItemAdded before any OutputTextDelta.
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
                // Transport broke — tear it down so next turn respawns.
                *guard = None;
                let _ = tx_event
                    .send(Err(ApiError::Stream(format!("clamp send failed: {e}"))))
                    .await;
                return;
            }

            // Read messages until the turn completes.
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
                        // Subprocess exited — tear down so next turn respawns.
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
                            .send(Err(ApiError::Stream(format!("clamp error: {e}"))))
                            .await;
                        break;
                    }
                }
            }
            // Don't shutdown — keep transport alive for next turn.
            // guard drops here, releasing the mutex.
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx_event);
        let response_stream = map_response_stream(stream, session_telemetry);
        Ok(response_stream)
    }

    /// Streams a turn via the Anthropic Messages API.
    ///
    /// This path is HTTP/SSE only — no WebSocket, no sticky routing, no incremental
    /// request reuse. Each follow-up sends full conversation history.
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

        // Build the neutral ABI turn request — same path as the HTTP Responses adapter.
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

    /// Returns `true` when `err` signals that the endpoint simply does not
    /// exist on this provider — i.e. we probed the wrong wire format.
    ///
    /// Conservative: only 404, 405, and 501 qualify. 400 ("bad request") is
    /// deliberately excluded because it usually means the payload is wrong,
    /// not that the endpoint is absent.
    fn is_wire_format_mismatch(err: &ChaosErr) -> bool {
        match err {
            ChaosErr::UnexpectedStatus(e) => matches!(e.status.as_u16(), 404 | 405 | 501),
            _ => false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// Streams a single model request within the current turn.
    ///
    /// The caller is responsible for passing per-turn settings explicitly (model selection,
    /// reasoning settings, telemetry context, and turn metadata). This method will prefer the
    /// Responses WebSocket transport when enabled and healthy, and will fall back to the HTTP
    /// Responses API transport otherwise.
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

        // Clamped mode: route through Claude Code subprocess.
        if self.client.state.clamped.load(Ordering::Relaxed) {
            return self
                .stream_clamped(prompt, model_info, session_telemetry)
                .await;
        }

        // Detect Anthropic wire format from the provider's base URL.
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

        // Chat Completions wire format.
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

        // Auto wire detection: resolve lazily using a Responses probe with
        // Chat Completions as the 404/405/501 fallback.
        if self.client.state.provider.wire_api == WireApi::Auto {
            // If a previous turn already resolved the wire, dispatch directly.
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

            // First attempt: probe with Responses API.
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
                    // Responses API answered — cache the result and return.
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

            // Fallback: Chat Completions.
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

        // Default: Responses wire format (OpenAI-compatible).
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

    fn resolve_anthropic_auth(&self) -> Result<AnthropicAuth> {
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

    /// Streams a turn via the OpenAI Chat Completions API (`/v1/chat/completions`).
    ///
    /// HTTP/SSE only — no WebSocket, no sticky routing, no incremental request
    /// reuse.  Each follow-up sends the full conversation history.
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_chat_completions_api",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = "chat_completions",
            transport = "chat_completions_http",
        )
    )]
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

/// Parses per-turn metadata into an HTTP header value.
///
/// Invalid values are treated as absent so callers can compare and propagate
/// metadata with the same sanitization path used when constructing headers.
fn parse_turn_metadata_header(turn_metadata_header: Option<&str>) -> Option<HeaderValue> {
    turn_metadata_header.and_then(|value| HeaderValue::from_str(value).ok())
}

fn tool_spec_to_abi_tool(tool: &ToolSpec) -> Option<AbiToolDef> {
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
///
/// These headers implement Codex-specific conventions:
///
/// - `x-codex-beta-features`: comma-separated beta feature keys enabled for the session.
/// - `x-codex-turn-state`: sticky routing token captured earlier in the turn.
/// - `x-codex-turn-metadata`: optional per-turn metadata for observability.
fn build_responses_headers(
    beta_features_header: Option<&str>,
    turn_state: Option<&Arc<OnceLock<String>>>,
    turn_metadata_header: Option<&HeaderValue>,
) -> ApiHeaderMap {
    let mut headers = ApiHeaderMap::new();
    if let Some(value) = beta_features_header
        && !value.is_empty()
        && let Ok(header_value) = HeaderValue::from_str(value)
    {
        headers.insert("x-codex-beta-features", header_value);
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

fn map_response_stream<S>(
    api_stream: S,
    session_telemetry: SessionTelemetry,
) -> ResponseStream
where
    S: futures::Stream<Item = std::result::Result<ResponseEvent, ApiError>>
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

/// Handles a 401 response by optionally refreshing ChatGPT tokens once.
///
/// When refresh succeeds, the caller should retry the API call; otherwise
/// the mapped `ChaosErr` is returned to the caller.
#[derive(Clone, Copy, Debug)]
struct UnauthorizedRecoveryExecution {
    mode: &'static str,
    phase: &'static str,
}

#[derive(Clone, Copy, Debug, Default)]
struct PendingUnauthorizedRetry {
    retry_after_unauthorized: bool,
    recovery_mode: Option<&'static str>,
    recovery_phase: Option<&'static str>,
}

impl PendingUnauthorizedRetry {
    fn from_recovery(recovery: UnauthorizedRecoveryExecution) -> Self {
        Self {
            retry_after_unauthorized: true,
            recovery_mode: Some(recovery.mode),
            recovery_phase: Some(recovery.phase),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct AuthRequestTelemetryContext {
    auth_mode: Option<&'static str>,
    auth_header_attached: bool,
    auth_header_name: Option<&'static str>,
    retry_after_unauthorized: bool,
    recovery_mode: Option<&'static str>,
    recovery_phase: Option<&'static str>,
}

impl AuthRequestTelemetryContext {
    fn new(
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

async fn handle_unauthorized(
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

    Err(map_api_error(ApiError::Transport(transport)))
}


struct ApiTelemetry {
    session_telemetry: SessionTelemetry,
    auth_context: AuthRequestTelemetryContext,
    request_route_telemetry: RequestRouteTelemetry,
}

impl ApiTelemetry {
    fn new(
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
        result: &std::result::Result<Option<std::result::Result<Event, BoxError>>, tokio::time::error::Elapsed>,
        duration: Duration,
    ) {
        self.session_telemetry.log_sse_event(result, duration);
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
