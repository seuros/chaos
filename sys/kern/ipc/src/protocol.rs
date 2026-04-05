//! Defines the protocol for a Codex session between a client and an agent.
//!
//! Uses a SQ (Submission Queue) / EQ (Event Queue) pattern to asynchronously communicate
//! between user and agent.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::ProcessId;
use crate::approvals::ElicitationRequestEvent;
use crate::config_types::ApprovalsReviewer;
use crate::config_types::CollaborationMode;
use crate::config_types::ModeKind;
use crate::config_types::Personality;
use crate::config_types::ReasoningSummary as ReasoningSummaryConfig;
use crate::config_types::ServiceTier;
use crate::config_types::WindowsSandboxLevel;
use crate::dynamic_tools::DynamicToolCallOutputContentItem;
use crate::dynamic_tools::DynamicToolCallRequest;
use crate::dynamic_tools::DynamicToolResponse;
use crate::items::TurnItem;
use crate::mcp::CallToolResult;
use crate::mcp::RequestId;
use crate::mcp::Resource as McpResource;
use crate::mcp::ResourceTemplate as McpResourceTemplate;
use crate::mcp::Tool as McpTool;
use crate::message_history::HistoryEntry;
use crate::models::MessagePhase;
use crate::models::ResponseItem;
use crate::models::WebSearchAction;
use crate::num_format::format_with_separators;
use crate::openai_models::ReasoningEffort as ReasoningEffortConfig;
use crate::parse_command::ParsedCommand;
use crate::plan_tool::UpdatePlanArgs;
use crate::request_permissions::RequestPermissionsEvent;
use crate::request_permissions::RequestPermissionsResponse;
use crate::request_user_input::RequestUserInputResponse;
use crate::user_input::UserInput;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_with::serde_as;
use strum_macros::Display;
use ts_rs::TS;

mod collab;
mod hooks;
mod policy;
mod review;
mod session;
mod skills;

pub use crate::approvals::ApplyPatchApprovalRequestEvent;
pub use crate::approvals::ElicitationAction;
pub use crate::approvals::ElicitationCompleteEvent;
pub use crate::approvals::ExecApprovalRequestEvent;
pub use crate::approvals::ExecApprovalRequestSkillMetadata;
pub use crate::approvals::ExecPolicyAmendment;

pub use crate::approvals::NetworkApprovalContext;
pub use crate::approvals::NetworkApprovalProtocol;
pub use crate::approvals::NetworkPolicyAmendment;
pub use crate::approvals::NetworkPolicyRuleAction;
pub use crate::permissions::FileSystemAccessMode;
pub use crate::permissions::FileSystemPath;
pub use crate::permissions::FileSystemSandboxEntry;
pub use crate::permissions::FileSystemSandboxKind;
pub use crate::permissions::FileSystemSandboxPolicy;
pub use crate::permissions::FileSystemSpecialPath;
pub use crate::permissions::NetworkSandboxPolicy;
pub use crate::request_permissions::RequestPermissionsArgs;
pub use crate::request_user_input::RequestUserInputEvent;
pub use collab::*;
pub use hooks::*;
pub use policy::*;
pub use review::*;
pub use session::*;
pub use skills::*;

/// Open/close tags for special user-input blocks. Used across crates to avoid
/// duplicated hardcoded strings.
pub const USER_INSTRUCTIONS_OPEN_TAG: &str = "<user_instructions>";
pub const USER_INSTRUCTIONS_CLOSE_TAG: &str = "</user_instructions>";
pub const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
pub const ENVIRONMENT_CONTEXT_CLOSE_TAG: &str = "</environment_context>";
pub const APPS_INSTRUCTIONS_OPEN_TAG: &str = "<apps_instructions>";
pub const APPS_INSTRUCTIONS_CLOSE_TAG: &str = "</apps_instructions>";
pub const SKILLS_INSTRUCTIONS_OPEN_TAG: &str = "<skills_instructions>";
pub const SKILLS_INSTRUCTIONS_CLOSE_TAG: &str = "</skills_instructions>";
pub const PLUGINS_INSTRUCTIONS_OPEN_TAG: &str = "<plugins_instructions>";
pub const PLUGINS_INSTRUCTIONS_CLOSE_TAG: &str = "</plugins_instructions>";
pub const COLLABORATION_MODE_OPEN_TAG: &str = "<collaboration_mode>";
pub const COLLABORATION_MODE_CLOSE_TAG: &str = "</collaboration_mode>";
pub const USER_MESSAGE_BEGIN: &str = "## My request for Codex:";

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

        /// Updated Windows sandbox mode for tool execution.
        #[serde(skip_serializing_if = "Option::is_none")]
        windows_sandbox_level: Option<WindowsSandboxLevel>,

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

    /// Request Codex to undo a turn (turn are stacked so it is the same effect as CMD + Z).
    Undo,

    /// Request Codex to drop the last N user turns from in-memory context.
    ///
    /// This does not attempt to revert local filesystem changes. Clients are
    /// responsible for undoing any edits on disk.
    #[serde(rename = "process_rollback")]
    ProcessRollback { num_turns: u32 },

    /// Request a code review from the agent.
    Review { review_request: ReviewRequest },

    /// Request to shut down codex instance.
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

macro_rules! impl_fromstr_via_serde {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl std::str::FromStr for $ty {
                type Err = serde_json::Error;
                fn from_str(s: &str) -> Result<Self, Self::Err> {
                    serde_json::from_str(s)
                }
            }
        )+
    };
}

impl_fromstr_via_serde!(FileSystemSandboxPolicy, NetworkSandboxPolicy);

/// Event Queue Entry - events from agent
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    /// Submission `id` that this event is correlated with.
    pub id: String,
    /// Payload
    pub msg: EventMsg,
}

/// Response event from the agent
/// NOTE: Make sure none of these values have optional types, as it will mess up the extension code-gen.
#[derive(Debug, Clone, Deserialize, Serialize, Display, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
#[strum(serialize_all = "snake_case")]
pub enum EventMsg {
    /// Error while executing a submission
    Error(ErrorEvent),

    /// Warning issued while processing a submission. Unlike `Error`, this
    /// indicates the turn continued but the user should still be notified.
    Warning(WarningEvent),

    /// Model routing changed from the requested model to a different model.
    ModelReroute(ModelRerouteEvent),

    /// Conversation history was compacted (either automatically or manually).
    ContextCompacted(ContextCompactedEvent),

    /// Conversation history was rolled back by dropping the last N user turns.
    #[serde(rename = "process_rolled_back")]
    ProcessRolledBack(ProcessRolledBackEvent),

    /// Agent has started a turn.
    #[serde(rename = "task_started")]
    TurnStarted(TurnStartedEvent),

    /// Agent has completed all actions.
    #[serde(rename = "task_complete")]
    TurnComplete(TurnCompleteEvent),

    /// Usage update for the current session, including totals and last turn.
    /// Optional means unknown — UIs should not display when `None`.
    TokenCount(TokenCountEvent),

    /// Agent text output message
    AgentMessage(AgentMessageEvent),

    /// User/system input message (what was sent to the model)
    UserMessage(UserMessageEvent),

    /// Reasoning event from agent.
    AgentReasoning(AgentReasoningEvent),

    /// Raw chain-of-thought from agent.
    AgentReasoningRawContent(AgentReasoningRawContentEvent),

    /// Signaled when the model begins a new reasoning summary section (e.g., a new titled block).
    AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent),

    /// Ack the client's configure message.
    SessionConfigured(SessionConfiguredEvent),

    /// Updated session metadata (e.g., process name changes).
    #[serde(rename = "process_name_updated")]
    ProcessNameUpdated(ProcessNameUpdatedEvent),

    /// Incremental MCP startup progress updates.
    McpStartupUpdate(McpStartupUpdateEvent),

    /// Aggregate MCP startup completion summary.
    McpStartupComplete(McpStartupCompleteEvent),

    McpToolCallBegin(McpToolCallBeginEvent),

    McpToolCallEnd(McpToolCallEndEvent),

    WebSearchBegin(WebSearchBeginEvent),

    WebSearchEnd(WebSearchEndEvent),

    ImageGenerationBegin(ImageGenerationBeginEvent),

    ImageGenerationEnd(ImageGenerationEndEvent),

    /// Notification that the server is about to execute a command.
    ExecCommandBegin(ExecCommandBeginEvent),

    /// Incremental chunk of output from a running command.
    ExecCommandOutputDelta(ExecCommandOutputDeltaEvent),

    /// Terminal interaction for an in-progress command (stdin sent and stdout observed).
    TerminalInteraction(TerminalInteractionEvent),

    ExecCommandEnd(ExecCommandEndEvent),

    /// Notification that the agent attached a local image via the view_image tool.
    ViewImageToolCall(ViewImageToolCallEvent),

    ExecApprovalRequest(ExecApprovalRequestEvent),

    RequestPermissions(RequestPermissionsEvent),

    RequestUserInput(RequestUserInputEvent),

    DynamicToolCallRequest(DynamicToolCallRequest),

    DynamicToolCallResponse(DynamicToolCallResponseEvent),

    ElicitationRequest(ElicitationRequestEvent),

    ElicitationComplete(ElicitationCompleteEvent),

    ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent),

    /// Notification advising the user that something they are using has been
    /// deprecated and should be phased out.
    DeprecationNotice(DeprecationNoticeEvent),

    BackgroundEvent(BackgroundEventEvent),

    UndoStarted(UndoStartedEvent),

    UndoCompleted(UndoCompletedEvent),

    /// Notification that a model stream experienced an error or disconnect
    /// and the system is handling it (e.g., retrying with backoff).
    StreamError(StreamErrorEvent),

    /// Notification that the agent is about to apply a code patch. Mirrors
    /// `ExecCommandBegin` so front‑ends can show progress indicators.
    PatchApplyBegin(PatchApplyBeginEvent),

    /// Notification that a patch application has finished.
    PatchApplyEnd(PatchApplyEndEvent),

    TurnDiff(TurnDiffEvent),

    /// Response to GetHistoryEntryRequest.
    GetHistoryEntryResponse(GetHistoryEntryResponseEvent),

    /// List of MCP tools available to the agent.
    McpListToolsResponse(McpListToolsResponseEvent),

    /// All tools visible to the model (builtins + arsenal + cron + MCP).
    AllToolsResponse(AllToolsResponseEvent),

    /// List of custom prompts available to the agent.
    ListCustomPromptsResponse(ListCustomPromptsResponseEvent),

    /// List of skills available to the agent.
    ListSkillsResponse(ListSkillsResponseEvent),

    /// List of remote skills available to the agent.
    ListRemoteSkillsResponse(ListRemoteSkillsResponseEvent),

    /// Remote skill downloaded to local cache.
    RemoteSkillDownloaded(RemoteSkillDownloadedEvent),

    /// Notification that skill data may have been updated and clients may want to reload.
    SkillsUpdateAvailable,

    PlanUpdate(UpdatePlanArgs),

    TurnAborted(TurnAbortedEvent),

    /// Notification that the agent is shutting down.
    ShutdownComplete,

    /// Entered review mode.
    EnteredReviewMode(ReviewRequest),

    /// Exited review mode with an optional final result to apply.
    ExitedReviewMode(ExitedReviewModeEvent),

    RawResponseItem(RawResponseItemEvent),

    ItemStarted(ItemStartedEvent),
    ItemCompleted(ItemCompletedEvent),
    HookStarted(HookStartedEvent),
    HookCompleted(HookCompletedEvent),

    AgentMessageContentDelta(AgentMessageContentDeltaEvent),
    PlanDelta(PlanDeltaEvent),
    ReasoningContentDelta(ReasoningContentDeltaEvent),
    ReasoningRawContentDelta(ReasoningRawContentDeltaEvent),

    /// Collab interaction: agent spawn begin.
    CollabAgentSpawnBegin(CollabAgentSpawnBeginEvent),
    /// Collab interaction: agent spawn end.
    CollabAgentSpawnEnd(CollabAgentSpawnEndEvent),
    /// Collab interaction: agent interaction begin.
    CollabAgentInteractionBegin(CollabAgentInteractionBeginEvent),
    /// Collab interaction: agent interaction end.
    CollabAgentInteractionEnd(CollabAgentInteractionEndEvent),
    /// Collab interaction: waiting begin.
    CollabWaitingBegin(CollabWaitingBeginEvent),
    /// Collab interaction: waiting end.
    CollabWaitingEnd(CollabWaitingEndEvent),
    /// Collab interaction: close begin.
    CollabCloseBegin(CollabCloseBeginEvent),
    /// Collab interaction: close end.
    CollabCloseEnd(CollabCloseEndEvent),
    /// Collab interaction: resume begin.
    CollabResumeBegin(CollabResumeBeginEvent),
    /// Collab interaction: resume end.
    CollabResumeEnd(CollabResumeEndEvent),
}

impl From<CollabAgentSpawnBeginEvent> for EventMsg {
    fn from(event: CollabAgentSpawnBeginEvent) -> Self {
        EventMsg::CollabAgentSpawnBegin(event)
    }
}

impl From<CollabAgentSpawnEndEvent> for EventMsg {
    fn from(event: CollabAgentSpawnEndEvent) -> Self {
        EventMsg::CollabAgentSpawnEnd(event)
    }
}

impl From<CollabAgentInteractionBeginEvent> for EventMsg {
    fn from(event: CollabAgentInteractionBeginEvent) -> Self {
        EventMsg::CollabAgentInteractionBegin(event)
    }
}

impl From<CollabAgentInteractionEndEvent> for EventMsg {
    fn from(event: CollabAgentInteractionEndEvent) -> Self {
        EventMsg::CollabAgentInteractionEnd(event)
    }
}

impl From<CollabWaitingBeginEvent> for EventMsg {
    fn from(event: CollabWaitingBeginEvent) -> Self {
        EventMsg::CollabWaitingBegin(event)
    }
}

impl From<CollabWaitingEndEvent> for EventMsg {
    fn from(event: CollabWaitingEndEvent) -> Self {
        EventMsg::CollabWaitingEnd(event)
    }
}

impl From<CollabCloseBeginEvent> for EventMsg {
    fn from(event: CollabCloseBeginEvent) -> Self {
        EventMsg::CollabCloseBegin(event)
    }
}

impl From<CollabCloseEndEvent> for EventMsg {
    fn from(event: CollabCloseEndEvent) -> Self {
        EventMsg::CollabCloseEnd(event)
    }
}

impl From<CollabResumeBeginEvent> for EventMsg {
    fn from(event: CollabResumeBeginEvent) -> Self {
        EventMsg::CollabResumeBegin(event)
    }
}

impl From<CollabResumeEndEvent> for EventMsg {
    fn from(event: CollabResumeEndEvent) -> Self {
        EventMsg::CollabResumeEnd(event)
    }
}

/// Codex errors that we expose to clients.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum CodexErrorInfo {
    ContextWindowExceeded,
    UsageLimitExceeded,
    ServerOverloaded,
    HttpConnectionFailed {
        http_status_code: Option<u16>,
    },
    /// Failed to connect to the response SSE stream.
    ResponseStreamConnectionFailed {
        http_status_code: Option<u16>,
    },
    InternalServerError,
    Unauthorized,
    BadRequest,
    SandboxError,
    /// The response SSE stream disconnected in the middle of a turnbefore completion.
    ResponseStreamDisconnected {
        http_status_code: Option<u16>,
    },
    /// Reached the retry limit for responses.
    ResponseTooManyFailedAttempts {
        http_status_code: Option<u16>,
    },
    #[serde(rename = "process_rollback_failed")]
    #[ts(rename = "process_rollback_failed")]
    ProcessRollbackFailed,
    Other,
}

impl CodexErrorInfo {
    /// Whether this error should mark the current turn as failed when replaying history.
    pub fn affects_turn_status(&self) -> bool {
        match self {
            Self::ProcessRollbackFailed => false,
            Self::ContextWindowExceeded
            | Self::UsageLimitExceeded
            | Self::ServerOverloaded
            | Self::HttpConnectionFailed { .. }
            | Self::ResponseStreamConnectionFailed { .. }
            | Self::InternalServerError
            | Self::Unauthorized
            | Self::BadRequest
            | Self::SandboxError
            | Self::ResponseStreamDisconnected { .. }
            | Self::ResponseTooManyFailedAttempts { .. }
            | Self::Other => true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct RawResponseItemEvent {
    pub item: ResponseItem,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ItemStartedEvent {
    pub process_id: ProcessId,
    pub turn_id: String,
    pub item: TurnItem,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ItemCompletedEvent {
    pub process_id: ProcessId,
    pub turn_id: String,
    pub item: TurnItem,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct AgentMessageContentDeltaEvent {
    pub process_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct PlanDeltaEvent {
    pub process_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ReasoningContentDeltaEvent {
    pub process_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ReasoningRawContentDeltaEvent {
    pub process_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub content_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExitedReviewModeEvent {
    pub review_output: Option<ReviewOutputEvent>,
}

// Individual event payload types matching each `EventMsg` variant.

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ErrorEvent {
    pub message: String,
    #[serde(default)]
    pub codex_error_info: Option<CodexErrorInfo>,
}

impl ErrorEvent {
    /// Whether this error should mark the current turn as failed when replaying history.
    pub fn affects_turn_status(&self) -> bool {
        self.codex_error_info
            .as_ref()
            .is_none_or(CodexErrorInfo::affects_turn_status)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WarningEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelRerouteReason {
    HighRiskCyberActivity,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ModelRerouteEvent {
    pub from_model: String,
    pub to_model: String,
    pub reason: ModelRerouteReason,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ContextCompactedEvent;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnCompleteEvent {
    pub turn_id: String,
    pub last_agent_message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnStartedEvent {
    pub turn_id: String,
    // TODO(aibrahim): make this not optional
    pub model_context_window: Option<i64>,
    #[serde(default)]
    pub collaboration_mode_kind: ModeKind,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq, JsonSchema, TS)]
pub struct TokenUsage {
    #[ts(type = "number")]
    pub input_tokens: i64,
    #[ts(type = "number")]
    pub cached_input_tokens: i64,
    #[ts(type = "number")]
    pub output_tokens: i64,
    #[ts(type = "number")]
    pub reasoning_output_tokens: i64,
    #[ts(type = "number")]
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct TokenUsageInfo {
    pub total_token_usage: TokenUsage,
    pub last_token_usage: TokenUsage,
    // TODO(aibrahim): make this not optional
    #[ts(type = "number | null")]
    pub model_context_window: Option<i64>,
}

impl TokenUsageInfo {
    pub fn new_or_append(
        info: &Option<TokenUsageInfo>,
        last: &Option<TokenUsage>,
        model_context_window: Option<i64>,
    ) -> Option<Self> {
        if info.is_none() && last.is_none() {
            return None;
        }

        let mut info = match info {
            Some(info) => info.clone(),
            None => Self {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window,
            },
        };
        if let Some(last) = last {
            info.append_last_usage(last);
        }
        if let Some(model_context_window) = model_context_window {
            info.model_context_window = Some(model_context_window);
        }
        Some(info)
    }

    pub fn append_last_usage(&mut self, last: &TokenUsage) {
        self.total_token_usage.add_assign(last);
        self.last_token_usage = last.clone();
    }

    pub fn fill_to_context_window(&mut self, context_window: i64) {
        let previous_total = self.total_token_usage.total_tokens;
        let delta = (context_window - previous_total).max(0);

        self.model_context_window = Some(context_window);
        self.total_token_usage = TokenUsage {
            total_tokens: context_window,
            ..TokenUsage::default()
        };
        self.last_token_usage = TokenUsage {
            total_tokens: delta,
            ..TokenUsage::default()
        };
    }

    pub fn full_context_window(context_window: i64) -> Self {
        let mut info = Self {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage::default(),
            model_context_window: Some(context_window),
        };
        info.fill_to_context_window(context_window);
        info
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TokenCountEvent {
    pub info: Option<TokenUsageInfo>,
    pub rate_limits: Option<RateLimitSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct RateLimitSnapshot {
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits: Option<CreditsSnapshot>,
    pub plan_type: Option<crate::account::PlanType>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct RateLimitWindow {
    /// Percentage (0-100) of the window that has been consumed.
    pub used_percent: f64,
    /// Rolling window duration, in minutes.
    #[ts(type = "number | null")]
    pub window_minutes: Option<i64>,
    /// Unix timestamp (seconds since epoch) when the window resets.
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct CreditsSnapshot {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

// Includes prompts, tools and space to call compact.
const BASELINE_TOKENS: i64 = 12000;

impl TokenUsage {
    pub fn is_zero(&self) -> bool {
        self.total_tokens == 0
    }

    pub fn cached_input(&self) -> i64 {
        self.cached_input_tokens.max(0)
    }

    pub fn non_cached_input(&self) -> i64 {
        (self.input_tokens - self.cached_input()).max(0)
    }

    /// Primary count for display as a single absolute value: non-cached input + output.
    pub fn blended_total(&self) -> i64 {
        (self.non_cached_input() + self.output_tokens.max(0)).max(0)
    }

    pub fn tokens_in_context_window(&self) -> i64 {
        self.total_tokens
    }

    /// Estimate the remaining user-controllable percentage of the model's context window.
    ///
    /// `context_window` is the total size of the model's context window.
    /// `BASELINE_TOKENS` should capture tokens that are always present in
    /// the context (e.g., system prompt and fixed tool instructions) so that
    /// the percentage reflects the portion the user can influence.
    ///
    /// This normalizes both the numerator and denominator by subtracting the
    /// baseline, so immediately after the first prompt the UI shows 100% left
    /// and trends toward 0% as the user fills the effective window.
    pub fn percent_of_context_window_remaining(&self, context_window: i64) -> i64 {
        if context_window <= BASELINE_TOKENS {
            return 0;
        }

        let effective_window = context_window - BASELINE_TOKENS;
        let used = (self.tokens_in_context_window() - BASELINE_TOKENS).max(0);
        let remaining = (effective_window - used).max(0);
        ((remaining as f64 / effective_window as f64) * 100.0)
            .clamp(0.0, 100.0)
            .round() as i64
    }

    /// In-place element-wise sum of token counts.
    pub fn add_assign(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FinalOutput {
    pub token_usage: TokenUsage,
}

impl From<TokenUsage> for FinalOutput {
    fn from(token_usage: TokenUsage) -> Self {
        Self { token_usage }
    }
}

impl fmt::Display for FinalOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let token_usage = &self.token_usage;

        write!(
            f,
            "Token usage: total={} input={}{} output={}{}",
            format_with_separators(token_usage.blended_total()),
            format_with_separators(token_usage.non_cached_input()),
            if token_usage.cached_input() > 0 {
                format!(
                    " (+ {} cached)",
                    format_with_separators(token_usage.cached_input())
                )
            } else {
                String::new()
            },
            format_with_separators(token_usage.output_tokens),
            if token_usage.reasoning_output_tokens > 0 {
                format!(
                    " (reasoning {})",
                    format_with_separators(token_usage.reasoning_output_tokens)
                )
            } else {
                String::new()
            }
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentMessageEvent {
    pub message: String,
    #[serde(default)]
    pub phase: Option<MessagePhase>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct UserMessageEvent {
    pub message: String,
    /// Image URLs sourced from `UserInput::Image`. These are safe
    /// to replay in legacy UI history events and correspond to images sent to
    /// the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    /// Local file paths sourced from `UserInput::LocalImage`. These are kept so
    /// the UI can reattach images when editing history, and should not be sent
    /// to the model or treated as API-ready URLs.
    #[serde(default)]
    pub local_images: Vec<std::path::PathBuf>,
    /// UI-defined spans within `message` used to render or persist special elements.
    #[serde(default)]
    pub text_elements: Vec<crate::user_input::TextElement>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningRawContentEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningSectionBreakEvent {
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpInvocation {
    /// Name of the MCP server as defined in the config.
    pub server: String,
    /// Name of the tool as given by the MCP server.
    pub tool: String,
    /// Arguments to the tool call.
    pub arguments: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpToolCallBeginEvent {
    /// Identifier so this can be paired with the McpToolCallEnd event.
    pub call_id: String,
    pub invocation: McpInvocation,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpToolCallEndEvent {
    /// Identifier for the corresponding McpToolCallBegin that finished.
    pub call_id: String,
    pub invocation: McpInvocation,
    #[ts(type = "string")]
    pub duration: Duration,
    /// Result of the tool call. Note this could be an error.
    pub result: Result<CallToolResult, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct DynamicToolCallResponseEvent {
    /// Identifier for the corresponding DynamicToolCallRequest.
    pub call_id: String,
    /// Turn ID that this dynamic tool call belongs to.
    pub turn_id: String,
    /// Dynamic tool name.
    pub tool: String,
    /// Dynamic tool call arguments.
    pub arguments: serde_json::Value,
    /// Dynamic tool response content items.
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
    /// Whether the tool call succeeded.
    pub success: bool,
    /// Optional error text when the tool call failed before producing a response.
    pub error: Option<String>,
    /// The duration of the dynamic tool call.
    #[ts(type = "string")]
    pub duration: Duration,
}

impl McpToolCallEndEvent {
    pub fn is_success(&self) -> bool {
        match &self.result {
            Ok(result) => !result.is_error.unwrap_or(false),
            Err(_) => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WebSearchBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WebSearchEndEvent {
    pub call_id: String,
    pub query: String,
    pub action: WebSearchAction,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ImageGenerationBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ImageGenerationEndEvent {
    pub call_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revised_prompt: Option<String>,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub saved_path: Option<String>,
}

#[derive(
    Debug, Clone, Copy, Display, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandSource {
    #[default]
    Agent,
    UserShell,
    UnifiedExecStartup,
    UnifiedExecInteraction,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandBeginEvent {
    /// Identifier so this can be paired with the ExecCommandEnd event.
    pub call_id: String,
    /// Identifier for the underlying PTY process (when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    /// Turn ID that this command belongs to.
    pub turn_id: String,
    /// The command to be executed.
    pub command: Vec<String>,
    /// The command's working directory if not the default cwd for the agent.
    pub cwd: PathBuf,
    pub parsed_cmd: Vec<ParsedCommand>,
    /// Where the command originated. Defaults to Agent for backward compatibility.
    #[serde(default)]
    pub source: ExecCommandSource,
    /// Raw input sent to a unified exec session (if this is an interaction event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandEndEvent {
    /// Identifier for the ExecCommandBegin that finished.
    pub call_id: String,
    /// Identifier for the underlying PTY process (when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    /// Turn ID that this command belongs to.
    pub turn_id: String,
    /// The command that was executed.
    pub command: Vec<String>,
    /// The command's working directory if not the default cwd for the agent.
    pub cwd: PathBuf,
    pub parsed_cmd: Vec<ParsedCommand>,
    /// Where the command originated. Defaults to Agent for backward compatibility.
    #[serde(default)]
    pub source: ExecCommandSource,
    /// Raw input sent to a unified exec session (if this is an interaction event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,

    /// Captured stdout
    pub stdout: String,
    /// Captured stderr
    pub stderr: String,
    /// Captured aggregated output
    #[serde(default)]
    pub aggregated_output: String,
    /// The command's exit code.
    pub exit_code: i32,
    /// The duration of the command execution.
    #[ts(type = "string")]
    pub duration: Duration,
    /// Formatted output from the command, as seen by the model.
    pub formatted_output: String,
    /// Completion status for this command execution.
    pub status: ExecCommandStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ViewImageToolCallEvent {
    /// Identifier for the originating tool call.
    pub call_id: String,
    /// Local filesystem path provided to the tool.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutputStream {
    Stdout,
    Stderr,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ExecCommandOutputDeltaEvent {
    /// Identifier for the ExecCommandBegin that produced this chunk.
    pub call_id: String,
    /// Which stream produced this chunk.
    pub stream: ExecOutputStream,
    /// Raw bytes from the stream (may not be valid UTF-8).
    #[serde_as(as = "serde_with::base64::Base64")]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    pub chunk: Vec<u8>,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct TerminalInteractionEvent {
    /// Identifier for the ExecCommandBegin that produced this chunk.
    pub call_id: String,
    /// Process id associated with the running command.
    pub process_id: String,
    /// Stdin sent to the running session.
    pub stdin: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct BackgroundEventEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct DeprecationNoticeEvent {
    /// Concise summary of what is deprecated.
    pub summary: String,
    /// Optional extra guidance, such as migration steps or rationale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct UndoStartedEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct UndoCompletedEvent {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ProcessRolledBackEvent {
    /// Number of user turns that were removed from context.
    pub num_turns: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct StreamErrorEvent {
    pub message: String,
    #[serde(default)]
    pub codex_error_info: Option<CodexErrorInfo>,
    /// Optional details about the underlying stream failure (often the same
    /// human-readable message that is surfaced as the terminal error if retries
    /// are exhausted).
    #[serde(default)]
    pub additional_details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct StreamInfoEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyBeginEvent {
    /// Identifier so this can be paired with the PatchApplyEnd event.
    pub call_id: String,
    /// Turn ID that this patch belongs to.
    /// Uses `#[serde(default)]` for backwards compatibility.
    #[serde(default)]
    pub turn_id: String,
    /// If true, there was no ApplyPatchApprovalRequest for this patch.
    pub auto_approved: bool,
    /// The changes to be applied.
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyEndEvent {
    /// Identifier for the PatchApplyBegin that finished.
    pub call_id: String,
    /// Turn ID that this patch belongs to.
    /// Uses `#[serde(default)]` for backwards compatibility.
    #[serde(default)]
    pub turn_id: String,
    /// Captured stdout (summary printed by apply_patch).
    pub stdout: String,
    /// Captured stderr (parser errors, IO failures, etc.).
    pub stderr: String,
    /// Whether the patch was applied successfully.
    pub success: bool,
    /// The changes that were applied (mirrors PatchApplyBeginEvent::changes).
    #[serde(default)]
    pub changes: HashMap<PathBuf, FileChange>,
    /// Completion status for this patch application.
    pub status: PatchApplyStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum PatchApplyStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnDiffEvent {
    pub unified_diff: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct GetHistoryEntryResponseEvent {
    pub offset: usize,
    pub log_id: u64,
    /// The entry at the requested offset, if available and parseable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<HistoryEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct McpListToolsResponseEvent {
    /// Fully qualified tool name -> tool definition.
    pub tools: std::collections::HashMap<String, McpTool>,
    /// Known resources grouped by server name.
    pub resources: std::collections::HashMap<String, Vec<McpResource>>,
    /// Known resource templates grouped by server name.
    pub resource_templates: std::collections::HashMap<String, Vec<McpResourceTemplate>>,
    /// Authentication status for each configured MCP server.
    pub auth_statuses: std::collections::HashMap<String, McpAuthStatus>,
}

/// A single tool entry in the all-tools response.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ToolSummary {
    /// Tool name as the model sees it.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Precomputed annotation labels for UI badges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotation_labels: Vec<String>,
    /// Optional structured tool annotations for UI rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub annotations: Option<serde_json::Value>,
    /// Origin: "builtin", "arsenal", "cron", or "mcp:<server>".
    pub source: String,
}

/// Response to `Op::ListAllTools`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AllToolsResponseEvent {
    pub tools: Vec<ToolSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct McpStartupUpdateEvent {
    /// Server name being started.
    pub server: String,
    /// Current startup status.
    pub status: McpStartupStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case", tag = "state")]
#[ts(rename_all = "snake_case", tag = "state")]
pub enum McpStartupStatus {
    Starting,
    Ready,
    Failed { error: String },
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, Default)]
pub struct McpStartupCompleteEvent {
    pub ready: Vec<String>,
    pub failed: Vec<McpStartupFailure>,
    pub cancelled: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct McpStartupFailure {
    pub server: String,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum McpAuthStatus {
    Unsupported,
    NotLoggedIn,
    BearerToken,
    OAuth,
}

impl fmt::Display for McpAuthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            McpAuthStatus::Unsupported => "Unsupported",
            McpAuthStatus::NotLoggedIn => "Not logged in",
            McpAuthStatus::BearerToken => "Bearer token",
            McpAuthStatus::OAuth => "OAuth",
        };
        f.write_str(text)
    }
}

/// User's decision in response to an ExecApprovalRequest.
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq, Display, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// User has approved this command and the agent should execute it.
    Approved,

    /// User has approved this command and wants to apply the proposed execpolicy
    /// amendment so future matching commands are permitted.
    ApprovedExecpolicyAmendment {
        proposed_execpolicy_amendment: ExecPolicyAmendment,
    },

    /// User has approved this request and wants future prompts in the same
    /// session-scoped approval cache to be automatically approved for the
    /// remainder of the session.
    ApprovedForSession,

    /// User chose to persist a network policy rule (allow/deny) for future
    /// requests to the same host.
    NetworkPolicyAmendment {
        network_policy_amendment: NetworkPolicyAmendment,
    },

    /// User has denied this command and the agent should not execute it, but
    /// it should continue the session and try something else.
    #[default]
    Denied,

    /// User has denied this command and the agent should not do anything until
    /// the user's next command.
    Abort,
}

impl ReviewDecision {
    /// Returns an opaque version of the decision without PII. We can't use an ignored flag
    /// on `serde` because the serialization is required by some surfaces.
    pub fn to_opaque_string(&self) -> &'static str {
        match self {
            ReviewDecision::Approved => "approved",
            ReviewDecision::ApprovedExecpolicyAmendment { .. } => "approved_with_amendment",
            ReviewDecision::ApprovedForSession => "approved_for_session",
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } => match network_policy_amendment.action {
                NetworkPolicyRuleAction::Allow => "approved_with_network_policy_allow",
                NetworkPolicyRuleAction::Deny => "denied_with_network_policy_deny",
            },
            ReviewDecision::Denied => "denied",
            ReviewDecision::Abort => "abort",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum FileChange {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct Chunk {
    /// 1-based line index of the first line in the original file
    pub orig_index: u32,
    pub deleted_lines: Vec<String>,
    pub inserted_lines: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnAbortedEvent {
    pub turn_id: Option<String>,
    pub reason: TurnAbortReason,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnAbortReason {
    Interrupted,
    Replaced,
    ReviewEnded,
}

#[cfg(test)]
mod tests;
