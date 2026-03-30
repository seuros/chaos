use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::RolloutItem;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EventPersistenceMode {
    #[default]
    Limited,
    Extended,
}

/// Whether a persisted session-history item should be recorded for the provided
/// persistence `mode`.
#[inline]
pub fn is_persisted_response_item(item: &RolloutItem, mode: EventPersistenceMode) -> bool {
    match item {
        RolloutItem::ResponseItem(item) => should_persist_response_item(item),
        RolloutItem::EventMsg(ev) => should_persist_event_msg(ev, mode),
        // Persist structural markers so replay and rollback remain stable.
        RolloutItem::Compacted(_) | RolloutItem::TurnContext(_) | RolloutItem::SessionMeta(_) => {
            true
        }
    }
}

#[inline]
pub fn should_persist_response_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::GhostSnapshot { .. }
        | ResponseItem::Compaction { .. } => true,
        ResponseItem::Other => false,
    }
}

/// Whether a `ResponseItem` should be persisted for the memories pipeline.
#[inline]
pub fn should_persist_response_item_for_memories(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { role, .. } => role != "developer",
        ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::WebSearchCall { .. } => true,
        ResponseItem::Reasoning { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::GhostSnapshot { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::Other => false,
    }
}

#[inline]
pub fn should_persist_event_msg(ev: &EventMsg, mode: EventPersistenceMode) -> bool {
    match mode {
        EventPersistenceMode::Limited => should_persist_event_msg_limited(ev),
        EventPersistenceMode::Extended => should_persist_event_msg_extended(ev),
    }
}

fn should_persist_event_msg_limited(ev: &EventMsg) -> bool {
    matches!(
        event_msg_persistence_mode(ev),
        Some(EventPersistenceMode::Limited)
    )
}

fn should_persist_event_msg_extended(ev: &EventMsg) -> bool {
    matches!(
        event_msg_persistence_mode(ev),
        Some(EventPersistenceMode::Limited) | Some(EventPersistenceMode::Extended)
    )
}

fn event_msg_persistence_mode(ev: &EventMsg) -> Option<EventPersistenceMode> {
    match ev {
        EventMsg::UserMessage(_)
        | EventMsg::AgentMessage(_)
        | EventMsg::AgentReasoning(_)
        | EventMsg::AgentReasoningRawContent(_)
        | EventMsg::TokenCount(_)
        | EventMsg::ContextCompacted(_)
        | EventMsg::EnteredReviewMode(_)
        | EventMsg::ExitedReviewMode(_)
        | EventMsg::ProcessRolledBack(_)
        | EventMsg::UndoCompleted(_)
        | EventMsg::TurnAborted(_)
        | EventMsg::TurnStarted(_)
        | EventMsg::TurnComplete(_) => Some(EventPersistenceMode::Limited),
        EventMsg::ItemCompleted(event) => {
            if matches!(event.item, chaos_ipc::items::TurnItem::Plan(_)) {
                Some(EventPersistenceMode::Limited)
            } else {
                None
            }
        }
        EventMsg::Error(_)
        | EventMsg::GuardianAssessment(_)
        | EventMsg::WebSearchEnd(_)
        | EventMsg::ExecCommandEnd(_)
        | EventMsg::PatchApplyEnd(_)
        | EventMsg::McpToolCallEnd(_)
        | EventMsg::ViewImageToolCall(_)
        | EventMsg::ImageGenerationEnd(_)
        | EventMsg::CollabAgentSpawnEnd(_)
        | EventMsg::CollabAgentInteractionEnd(_)
        | EventMsg::CollabWaitingEnd(_)
        | EventMsg::CollabCloseEnd(_)
        | EventMsg::CollabResumeEnd(_)
        | EventMsg::DynamicToolCallRequest(_)
        | EventMsg::DynamicToolCallResponse(_) => Some(EventPersistenceMode::Extended),
        EventMsg::Warning(_)
        | EventMsg::ModelReroute(_)
        | EventMsg::AgentMessageDelta(_)
        | EventMsg::AgentReasoningDelta(_)
        | EventMsg::AgentReasoningRawContentDelta(_)
        | EventMsg::AgentReasoningSectionBreak(_)
        | EventMsg::RawResponseItem(_)
        | EventMsg::SessionConfigured(_)
        | EventMsg::ProcessNameUpdated(_)
        | EventMsg::McpToolCallBegin(_)
        | EventMsg::WebSearchBegin(_)
        | EventMsg::ExecCommandBegin(_)
        | EventMsg::TerminalInteraction(_)
        | EventMsg::ExecCommandOutputDelta(_)
        | EventMsg::ExecApprovalRequest(_)
        | EventMsg::RequestPermissions(_)
        | EventMsg::RequestUserInput(_)
        | EventMsg::ElicitationRequest(_)
        | EventMsg::ElicitationComplete(_)
        | EventMsg::ApplyPatchApprovalRequest(_)
        | EventMsg::BackgroundEvent(_)
        | EventMsg::StreamError(_)
        | EventMsg::PatchApplyBegin(_)
        | EventMsg::TurnDiff(_)
        | EventMsg::GetHistoryEntryResponse(_)
        | EventMsg::UndoStarted(_)
        | EventMsg::McpListToolsResponse(_)
        | EventMsg::McpStartupUpdate(_)
        | EventMsg::McpStartupComplete(_)
        | EventMsg::ListCustomPromptsResponse(_)
        | EventMsg::ListSkillsResponse(_)
        | EventMsg::ListRemoteSkillsResponse(_)
        | EventMsg::RemoteSkillDownloaded(_)
        | EventMsg::PlanUpdate(_)
        | EventMsg::ShutdownComplete
        | EventMsg::DeprecationNotice(_)
        | EventMsg::ItemStarted(_)
        | EventMsg::HookStarted(_)
        | EventMsg::HookCompleted(_)
        | EventMsg::AgentMessageContentDelta(_)
        | EventMsg::PlanDelta(_)
        | EventMsg::ReasoningContentDelta(_)
        | EventMsg::ReasoningRawContentDelta(_)
        | EventMsg::SkillsUpdateAvailable
        | EventMsg::CollabAgentSpawnBegin(_)
        | EventMsg::CollabAgentInteractionBegin(_)
        | EventMsg::CollabWaitingBegin(_)
        | EventMsg::CollabCloseBegin(_)
        | EventMsg::CollabResumeBegin(_)
        | EventMsg::ImageGenerationBegin(_) => None,
    }
}
