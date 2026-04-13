use crate::approvals::ApplyPatchApprovalRequestEvent;
use crate::approvals::ElicitationCompleteEvent;
use crate::approvals::ElicitationRequestEvent;
use crate::approvals::ExecApprovalRequestEvent;
use crate::dynamic_tools::DynamicToolCallRequest;
use crate::message_history::HistoryEntry;
use crate::plan_tool::UpdatePlanArgs;
use crate::request_permissions::RequestPermissionsEvent;
use crate::request_user_input::RequestUserInputEvent;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use ts_rs::TS;

use super::AgentMessageContentDeltaEvent;
use super::AgentMessageEvent;
use super::AgentReasoningEvent;
use super::AgentReasoningRawContentEvent;
use super::AgentReasoningSectionBreakEvent;
use super::AllToolsResponseEvent;
use super::BackgroundEventEvent;
use super::ContextCompactedEvent;
use super::DeprecationNoticeEvent;
use super::DynamicToolCallResponseEvent;
use super::ErrorEvent;
use super::ExecCommandBeginEvent;
use super::ExecCommandEndEvent;
use super::ExecCommandOutputDeltaEvent;
use super::ExitedReviewModeEvent;
use super::HookCompletedEvent;
use super::HookStartedEvent;
use super::ImageGenerationBeginEvent;
use super::ImageGenerationEndEvent;
use super::ItemCompletedEvent;
use super::ItemStartedEvent;
use super::ListCustomPromptsResponseEvent;
use super::ListRemoteSkillsResponseEvent;
use super::ListSkillsResponseEvent;
use super::McpListToolsResponseEvent;
use super::McpStartupCompleteEvent;
use super::McpStartupUpdateEvent;
use super::McpToolCallBeginEvent;
use super::McpToolCallEndEvent;
use super::ModelRerouteEvent;
use super::PatchApplyBeginEvent;
use super::PatchApplyEndEvent;
use super::PlanDeltaEvent;
use super::ProcessNameUpdatedEvent;
use super::ProcessRolledBackEvent;
use super::RawResponseItemEvent;
use super::ReasoningContentDeltaEvent;
use super::ReasoningRawContentDeltaEvent;
use super::RemoteSkillDownloadedEvent;
use super::ReviewRequest;
use super::SessionConfiguredEvent;
use super::StreamErrorEvent;
use super::TerminalInteractionEvent;
use super::TokenCountEvent;
use super::TurnAbortedEvent;
use super::TurnCompleteEvent;
use super::TurnDiffEvent;
use super::TurnStartedEvent;
use super::UndoCompletedEvent;
use super::UndoStartedEvent;
use super::UserMessageEvent;
use super::ViewImageToolCallEvent;
use super::WarningEvent;
use super::WebSearchBeginEvent;
use super::WebSearchEndEvent;

use super::CollabAgentInteractionBeginEvent;
use super::CollabAgentInteractionEndEvent;
use super::CollabAgentSpawnBeginEvent;
use super::CollabAgentSpawnEndEvent;
use super::CollabCloseBeginEvent;
use super::CollabCloseEndEvent;
use super::CollabResumeBeginEvent;
use super::CollabResumeEndEvent;
use super::CollabWaitingBeginEvent;
use super::CollabWaitingEndEvent;

/// Event Queue Entry - events from agent
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    /// Submission `id` that this event is correlated with.
    pub id: String,
    /// Payload
    pub msg: EventMsg,
}

/// Response to GetHistoryEntryRequest.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct GetHistoryEntryResponseEvent {
    pub offset: usize,
    pub log_id: u64,
    /// The entry at the requested offset, if available and parseable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<HistoryEntry>,
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
