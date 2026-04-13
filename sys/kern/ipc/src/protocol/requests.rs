use std::path::PathBuf;

use crate::approvals::ElicitationAction;
use crate::config_types::ApprovalsReviewer;
use crate::config_types::CollaborationMode;
use crate::config_types::Personality;
use crate::config_types::ReasoningSummary as ReasoningSummaryConfig;
use crate::config_types::ServiceTier;
use crate::dynamic_tools::DynamicToolResponse;
use crate::mcp::RequestId;
use crate::openai_models::ReasoningEffort as ReasoningEffortConfig;
use crate::request_permissions::RequestPermissionsResponse;
use crate::request_user_input::RequestUserInputResponse;
use crate::user_input::UserInput;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

use super::ApprovalPolicy;
use super::RemoteSkillHazelnutScope;
use super::RemoteSkillProductSurface;
use super::ReviewDecision;
use super::ReviewRequest;
use super::SandboxPolicy;

/// Submission Queue Entry - requests from user
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct Submission {
    /// Unique id for this Submission to correlate with Events
    pub id: String,
    /// Payload
    pub op: Op,
    /// Optional W3C trace carrier propagated across async submission handoffs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<W3cTraceContext>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct W3cTraceContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tracestate: Option<String>,
}

/// Config payload for refreshing MCP servers.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct McpServerRefreshConfig {
    pub mcp_servers: Value,
    pub mcp_oauth_credentials_store_mode: Value,
}

/// Submission operation
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
#[non_exhaustive]
pub enum Op {
    /// Abort current task without terminating background terminal processes.
    /// This server sends [`EventMsg::TurnAborted`] in response.
    Interrupt,

    /// Terminate all running background terminal processes for this thread.
    /// Use this when callers intentionally want to stop long-lived background shells.
    CleanBackgroundTerminals,

    /// Legacy user input.
    ///
    /// Prefer [`Op::UserTurn`] so the caller provides full turn context
    /// (cwd/approval/sandbox/model/etc.) for each turn.
    UserInput {
        /// User input items, see `InputItem`
        items: Vec<UserInput>,
        /// Optional JSON Schema used to constrain the final assistant message for this turn.
        #[serde(skip_serializing_if = "Option::is_none")]
        final_output_json_schema: Option<Value>,
    },

    /// Similar to [`Op::UserInput`], but contains additional context required
    /// for a Chaos process turn.
    UserTurn {
        /// User input items, see `InputItem`
        items: Vec<UserInput>,

        /// `cwd` to use with the [`SandboxPolicy`] and potentially tool calls
        /// such as `local_shell`.
        cwd: PathBuf,

        /// Policy to use for command approval.
        approval_policy: ApprovalPolicy,

        /// Policy to use for tool calls such as `local_shell`.
        sandbox_policy: SandboxPolicy,

        /// Must be a valid model slug for the configured client session
        /// associated with this conversation.
        model: String,

        /// Will only be honored if the model is configured to use reasoning.
        #[serde(skip_serializing_if = "Option::is_none")]
        effort: Option<ReasoningEffortConfig>,

        /// Will only be honored if the model is configured to use reasoning.
        ///
        /// When omitted, the session keeps the current setting (which allows core to
        /// fall back to the selected model's default on new sessions).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<ReasoningSummaryConfig>,

        /// Optional service tier override for this turn.
        ///
        /// Use `Some(Some(_))` to set a specific tier for this turn, `Some(None)` to
        /// explicitly clear the tier for this turn, or `None` to keep the existing
        /// session preference.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service_tier: Option<Option<ServiceTier>>,

        // The JSON schema to use for the final assistant message
        final_output_json_schema: Option<Value>,

        /// EXPERIMENTAL - set a pre-set collaboration mode.
        /// Takes precedence over model, effort, and developer instructions if set.
        #[serde(skip_serializing_if = "Option::is_none")]
        collaboration_mode: Option<CollaborationMode>,

        /// Optional personality override for this turn.
        #[serde(skip_serializing_if = "Option::is_none")]
        personality: Option<Personality>,
    },

    /// Override parts of the persistent turn context for subsequent turns.
    ///
    /// All fields are optional; when omitted, the existing value is preserved.
    /// This does not enqueue any input – it only updates defaults used for
    /// turns that rely on persistent session-level context (for example,
    /// [`Op::UserInput`]).
    OverrideTurnContext {
        /// Updated `cwd` for sandbox/tool calls.
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,

        /// Updated command approval policy.
        #[serde(skip_serializing_if = "Option::is_none")]
        approval_policy: Option<ApprovalPolicy>,

        /// Updated approval reviewer for future approval prompts.
        #[serde(skip_serializing_if = "Option::is_none")]
        approvals_reviewer: Option<ApprovalsReviewer>,

        /// Updated sandbox policy for tool calls.
        #[serde(skip_serializing_if = "Option::is_none")]
        sandbox_policy: Option<SandboxPolicy>,

        /// Updated model slug. When set, the model info is derived
        /// automatically.
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,

        /// Updated reasoning effort (honored only for reasoning-capable models).
        ///
        /// Use `Some(Some(_))` to set a specific effort, `Some(None)` to clear
        /// the effort, or `None` to leave the existing value unchanged.
        #[serde(skip_serializing_if = "Option::is_none")]
        effort: Option<Option<ReasoningEffortConfig>>,

        /// Updated reasoning summary preference (honored only for reasoning-capable models).
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<ReasoningSummaryConfig>,

        /// Updated service tier preference for future turns.
        ///
        /// Use `Some(Some(_))` to set a specific tier, `Some(None)` to clear the
        /// preference, or `None` to leave the existing value unchanged.
        #[serde(skip_serializing_if = "Option::is_none")]
        service_tier: Option<Option<ServiceTier>>,

        /// EXPERIMENTAL - set a pre-set collaboration mode.
        /// Takes precedence over model, effort, and developer instructions if set.
        #[serde(skip_serializing_if = "Option::is_none")]
        collaboration_mode: Option<CollaborationMode>,

        /// Updated personality preference.
        #[serde(skip_serializing_if = "Option::is_none")]
        personality: Option<Personality>,
    },

    /// Toggle clamped mode (Claude Code subprocess as transport).
    SetClamped { enabled: bool },

    /// Approve a command execution
    ExecApproval {
        /// The id of the submission we are approving
        id: String,
        /// Turn id associated with the approval event, when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        /// The user's decision in response to the request.
        decision: ReviewDecision,
    },

    /// Approve a code patch
    PatchApproval {
        /// The id of the submission we are approving
        id: String,
        /// The user's decision in response to the request.
        decision: ReviewDecision,
    },

    /// Resolve an MCP elicitation request.
    ResolveElicitation {
        /// Name of the MCP server that issued the request.
        server_name: String,
        /// Request identifier from the MCP server.
        request_id: RequestId,
        /// User's decision for the request.
        decision: ElicitationAction,
        /// Structured user input supplied for accepted elicitations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
        /// Optional client metadata associated with the elicitation response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
    },

    /// Resolve a request_user_input tool call.
    #[serde(rename = "user_input_answer")]
    UserInputAnswer {
        /// Turn id for the in-flight request.
        id: String,
        /// User-provided answers.
        response: RequestUserInputResponse,
    },

    /// Resolve a request_permissions tool call.
    RequestPermissionsResponse {
        /// Call id for the in-flight request.
        id: String,
        /// User-granted permissions.
        response: RequestPermissionsResponse,
    },

    /// Resolve a dynamic tool call request.
    DynamicToolResponse {
        /// Call id for the in-flight request.
        id: String,
        /// Tool output payload.
        response: DynamicToolResponse,
    },

    /// Append an entry to the persistent cross-session message history.
    ///
    /// Note the entry is not guaranteed to be logged if the user has
    /// history disabled, it matches the list of "sensitive" patterns, etc.
    AddToHistory {
        /// The message text to be stored.
        text: String,
    },

    /// Request a single history entry identified by `log_id` + `offset`.
    GetHistoryEntryRequest { offset: usize, log_id: u64 },

    /// Request the list of MCP tools available across all configured servers.
    /// Reply is delivered via `EventMsg::McpListToolsResponse`.
    ListMcpTools,

    /// Request all tools visible to the model (builtins + arsenal + cron + MCP).
    /// Reply is delivered via `EventMsg::AllToolsResponse`.
    ListAllTools,

    /// Request MCP servers to reinitialize and refresh cached tool lists.
    RefreshMcpServers { config: McpServerRefreshConfig },

    /// Reload user config layer overrides for the active session.
    ///
    /// This updates runtime config-derived behavior (for example app
    /// enable/disable state) without restarting the thread.
    ReloadUserConfig,

    /// Request the list of available custom prompts.
    ListCustomPrompts,

    /// Request the list of skills for the provided `cwd` values or the session default.
    ListSkills {
        /// Working directories to scope repo skills discovery.
        ///
        /// When empty, the session default working directory is used.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cwds: Vec<PathBuf>,

        /// When true, recompute skills even if a cached result exists.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        force_reload: bool,
    },

    /// Request the list of remote skills available via ChatGPT sharing.
    ListRemoteSkills {
        hazelnut_scope: RemoteSkillHazelnutScope,
        product_surface: RemoteSkillProductSurface,
        enabled: Option<bool>,
    },

    /// Download a remote skill by id into the local skills cache.
    DownloadRemoteSkill { hazelnut_id: String },

    /// Request the agent to summarize the current conversation context.
    /// The agent will use its existing context (either conversation history or previous response id)
    /// to generate a summary which will be returned as an AgentMessage event.
    Compact,

    /// Drop all persisted memory artifacts and memory-tracking DB rows.
    DropMemories,

    /// Trigger a single pass of the startup memory pipeline.
    UpdateMemories,

    /// Set a user-facing process name in persisted session metadata.
    /// This is a local-only operation handled by chaos-kern; it does not
    /// involve the model.
    #[serde(rename = "set_process_name")]
    SetProcessName { name: String },

    /// Request Chaos to undo a turn (turn are stacked so it is the same effect as CMD + Z).
    Undo,

    /// Request Chaos to drop the last N user turns from in-memory context.
    ///
    /// This does not attempt to revert local filesystem changes. Clients are
    /// responsible for undoing any edits on disk.
    #[serde(rename = "process_rollback")]
    ProcessRollback { num_turns: u32 },

    /// Request a code review from the agent.
    Review { review_request: ReviewRequest },

    /// Request to shut down chaos instance.
    Shutdown,

    /// Execute a user-initiated one-off shell command (triggered by "!cmd").
    ///
    /// The command string is executed using the user's default shell and may
    /// include shell syntax (pipes, redirects, etc.). Output is streamed via
    /// `ExecCommand*` events and the UI regains control upon `TurnComplete`.
    RunUserShellCommand {
        /// The raw command string after '!'
        command: String,
    },

    /// Request the list of available models.
    ListModels,
}

impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::CleanBackgroundTerminals => "clean_background_terminals",
            Self::UserInput { .. } => "user_input",
            Self::UserTurn { .. } => "user_turn",
            Self::OverrideTurnContext { .. } => "override_turn_context",
            Self::SetClamped { .. } => "set_clamped",
            Self::ExecApproval { .. } => "exec_approval",
            Self::PatchApproval { .. } => "patch_approval",
            Self::ResolveElicitation { .. } => "resolve_elicitation",
            Self::UserInputAnswer { .. } => "user_input_answer",
            Self::RequestPermissionsResponse { .. } => "request_permissions_response",
            Self::DynamicToolResponse { .. } => "dynamic_tool_response",
            Self::AddToHistory { .. } => "add_to_history",
            Self::GetHistoryEntryRequest { .. } => "get_history_entry_request",
            Self::ListMcpTools => "list_mcp_tools",
            Self::ListAllTools => "list_all_tools",
            Self::RefreshMcpServers { .. } => "refresh_mcp_servers",
            Self::ReloadUserConfig => "reload_user_config",
            Self::ListCustomPrompts => "list_custom_prompts",
            Self::ListSkills { .. } => "list_skills",
            Self::ListRemoteSkills { .. } => "list_remote_skills",
            Self::DownloadRemoteSkill { .. } => "download_remote_skill",
            Self::Compact => "compact",
            Self::DropMemories => "drop_memories",
            Self::UpdateMemories => "update_memories",
            Self::SetProcessName { .. } => "set_process_name",
            Self::Undo => "undo",
            Self::ProcessRollback { .. } => "process_rollback",
            Self::Review { .. } => "review",
            Self::Shutdown => "shutdown",
            Self::RunUserShellCommand { .. } => "run_user_shell_command",
            Self::ListModels => "list_models",
        }
    }
}
