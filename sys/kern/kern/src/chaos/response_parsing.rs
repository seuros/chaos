use std::collections::HashMap;
use std::collections::HashSet;

use chaos_ipc::items::PlanItem as IpcPlanItem;
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::AgentMessageContentDeltaEvent;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::PlanDeltaEvent;
use chaos_lex::AssistantTextChunk;
use chaos_lex::AssistantTextStreamParser;
use chaos_lex::ProposedPlanSegment;
use chaos_lex::extract_proposed_plan_text;
use chaos_lex::strip_citations;
use futures::future::BoxFuture;
use futures::prelude::*;
use futures::stream::FuturesOrdered;
use state_machines::state_machine;

use super::Session;
use super::TurnContext;
use crate::error::Result as ChaosResult;
use crate::stream_events_utils::handle_non_tool_response_item;
use crate::stream_events_utils::last_assistant_message_from_item;
use crate::stream_events_utils::record_completed_response_item;
use crate::util::error_or_panic;

// State machine for a proposed plan item lifecycle.
// Transitions: Idle → Started → Completed
state_machine! {
    name: PlanItemMachine,
    initial: Idle,
    states: [Idle, Started, Completed],
    events {
        start {
            transition: { from: Idle, to: Started }
        }
        complete {
            transition: { from: Started, to: Completed }
        }
    }
}

/// Ephemeral per-response state for streaming a single proposed plan item.
/// This is intentionally not persisted or stored in session/state since it
/// only exists while a response is actively streaming. The final plan text
/// is extracted from the completed assistant message.
pub(super) struct ProposedPlanItemState {
    pub(super) item_id: String,
    machine: DynamicPlanItemMachine<()>,
}

impl ProposedPlanItemState {
    pub(super) fn new(turn_id: &str) -> Self {
        Self {
            item_id: format!("{turn_id}-plan"),
            machine: DynamicPlanItemMachine::new(()),
        }
    }

    pub(super) fn started(&self) -> bool {
        matches!(self.machine.current_state(), "Started" | "Completed")
    }

    pub(super) fn completed(&self) -> bool {
        self.machine.current_state() == "Completed"
    }

    pub(super) async fn start(&mut self, sess: &Session, turn_context: &TurnContext) {
        if self.started() || self.completed() {
            return;
        }
        let _ = self.machine.handle(PlanItemMachineEvent::Start);
        let item = TurnItem::Plan(IpcPlanItem {
            id: self.item_id.clone(),
            text: String::new(),
        });
        sess.emit_turn_item_started(turn_context, &item).await;
    }

    pub(super) async fn push_delta(
        &mut self,
        sess: &Session,
        turn_context: &TurnContext,
        delta: &str,
    ) {
        if self.completed() {
            return;
        }
        if delta.is_empty() {
            return;
        }
        let event = PlanDeltaEvent {
            process_id: sess.conversation_id.to_string(),
            turn_id: turn_context.sub_id.clone(),
            item_id: self.item_id.clone(),
            delta: delta.to_string(),
        };
        sess.send_event(turn_context, EventMsg::PlanDelta(event))
            .await;
    }

    pub(super) async fn complete_with_text(
        &mut self,
        sess: &Session,
        turn_context: &TurnContext,
        text: String,
    ) {
        if self.completed() || !self.started() {
            return;
        }
        let _ = self.machine.handle(PlanItemMachineEvent::Complete);
        let item = TurnItem::Plan(IpcPlanItem {
            id: self.item_id.clone(),
            text,
        });
        sess.emit_turn_item_completed(turn_context, item).await;
    }
}

/// Aggregated state used only while streaming a plan-mode response.
/// Includes per-item parsers, deferred agent message bookkeeping, and the
/// plan item lifecycle tracked via the PlanItem state machine.
pub(super) struct PlanModeStreamState {
    /// Agent message items started by the model but deferred until we see
    /// non-plan text.
    pub(super) pending_agent_message_items: HashMap<String, TurnItem>,
    /// Agent message items whose start notification has been emitted.
    pub(super) started_agent_message_items: HashSet<String>,
    /// Leading whitespace buffered until we see non-whitespace text for an
    /// item.
    pub(super) leading_whitespace_by_item: HashMap<String, String>,
    /// Tracks plan item lifecycle while streaming plan output.
    pub(super) plan_item_state: ProposedPlanItemState,
}

impl PlanModeStreamState {
    pub(super) fn new(turn_id: &str) -> Self {
        Self {
            pending_agent_message_items: HashMap::new(),
            started_agent_message_items: HashSet::new(),
            leading_whitespace_by_item: HashMap::new(),
            plan_item_state: ProposedPlanItemState::new(turn_id),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct AssistantMessageStreamParsers {
    plan_mode: bool,
    parsers_by_item: HashMap<String, AssistantTextStreamParser>,
}

pub(super) type ParsedAssistantTextDelta = AssistantTextChunk;

impl AssistantMessageStreamParsers {
    pub(super) fn new(plan_mode: bool) -> Self {
        Self {
            plan_mode,
            parsers_by_item: HashMap::new(),
        }
    }

    pub(super) fn parser_mut(&mut self, item_id: &str) -> &mut AssistantTextStreamParser {
        let plan_mode = self.plan_mode;
        self.parsers_by_item
            .entry(item_id.to_string())
            .or_insert_with(|| AssistantTextStreamParser::new(plan_mode))
    }

    pub(super) fn seed_item_text(&mut self, item_id: &str, text: &str) -> ParsedAssistantTextDelta {
        if text.is_empty() {
            return ParsedAssistantTextDelta::default();
        }
        self.parser_mut(item_id).push_str(text)
    }

    pub(super) fn parse_delta(&mut self, item_id: &str, delta: &str) -> ParsedAssistantTextDelta {
        self.parser_mut(item_id).push_str(delta)
    }

    pub(super) fn finish_item(&mut self, item_id: &str) -> ParsedAssistantTextDelta {
        let Some(mut parser) = self.parsers_by_item.remove(item_id) else {
            return ParsedAssistantTextDelta::default();
        };
        parser.finish()
    }

    pub(super) fn drain_finished(&mut self) -> Vec<(String, ParsedAssistantTextDelta)> {
        let parsers_by_item = std::mem::take(&mut self.parsers_by_item);
        parsers_by_item
            .into_iter()
            .map(|(item_id, mut parser)| (item_id, parser.finish()))
            .collect()
    }
}

/// In plan mode we defer agent message starts until the parser emits non-plan
/// text. The parser buffers each line until it can rule out a tag prefix, so
/// plan-only outputs never show up as empty assistant messages.
pub(super) async fn maybe_emit_pending_agent_message_start(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item_id: &str,
) {
    if state.started_agent_message_items.contains(item_id) {
        return;
    }
    if let Some(item) = state.pending_agent_message_items.remove(item_id) {
        sess.emit_turn_item_started(turn_context, &item).await;
        state
            .started_agent_message_items
            .insert(item_id.to_string());
    }
}

/// Agent messages are text-only today; concatenate all text entries.
pub(super) fn agent_message_text(item: &chaos_ipc::items::AgentMessageItem) -> String {
    item.content
        .iter()
        .map(|entry| match entry {
            chaos_ipc::items::AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect()
}

/// Split the stream into normal assistant text vs. proposed plan content.
/// Normal text becomes AgentMessage deltas; plan content becomes PlanDelta +
/// TurnItem::Plan.
pub(super) async fn handle_plan_segments(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item_id: &str,
    segments: Vec<ProposedPlanSegment>,
) {
    for segment in segments {
        match segment {
            ProposedPlanSegment::Normal(delta) => {
                if delta.is_empty() {
                    continue;
                }
                let has_non_whitespace = delta.chars().any(|ch| !ch.is_whitespace());
                if !has_non_whitespace && !state.started_agent_message_items.contains(item_id) {
                    let entry = state
                        .leading_whitespace_by_item
                        .entry(item_id.to_string())
                        .or_default();
                    entry.push_str(&delta);
                    continue;
                }
                let delta = if !state.started_agent_message_items.contains(item_id) {
                    if let Some(prefix) = state.leading_whitespace_by_item.remove(item_id) {
                        format!("{prefix}{delta}")
                    } else {
                        delta
                    }
                } else {
                    delta
                };
                maybe_emit_pending_agent_message_start(sess, turn_context, state, item_id).await;

                let event = AgentMessageContentDeltaEvent {
                    process_id: sess.conversation_id.to_string(),
                    turn_id: turn_context.sub_id.clone(),
                    item_id: item_id.to_string(),
                    delta,
                };
                sess.send_event(turn_context, EventMsg::AgentMessageContentDelta(event))
                    .await;
            }
            ProposedPlanSegment::ProposedPlanStart => {
                if !state.plan_item_state.completed() {
                    state.plan_item_state.start(sess, turn_context).await;
                }
            }
            ProposedPlanSegment::ProposedPlanDelta(delta) => {
                if !state.plan_item_state.completed() {
                    if !state.plan_item_state.started() {
                        state.plan_item_state.start(sess, turn_context).await;
                    }
                    state
                        .plan_item_state
                        .push_delta(sess, turn_context, &delta)
                        .await;
                }
            }
            ProposedPlanSegment::ProposedPlanEnd => {}
        }
    }
}

pub(super) async fn emit_streamed_assistant_text_delta(
    sess: &Session,
    turn_context: &TurnContext,
    plan_mode_state: Option<&mut PlanModeStreamState>,
    item_id: &str,
    parsed: ParsedAssistantTextDelta,
) {
    if parsed.is_empty() {
        return;
    }
    if !parsed.citations.is_empty() {
        // Citation extraction is intentionally local for now; we strip citations
        // from display text but do not yet surface them in protocol events.
        let _citations = parsed.citations;
    }
    if let Some(state) = plan_mode_state {
        if !parsed.plan_segments.is_empty() {
            handle_plan_segments(sess, turn_context, state, item_id, parsed.plan_segments).await;
        }
        return;
    }
    if parsed.visible_text.is_empty() {
        return;
    }
    let event = AgentMessageContentDeltaEvent {
        process_id: sess.conversation_id.to_string(),
        turn_id: turn_context.sub_id.clone(),
        item_id: item_id.to_string(),
        delta: parsed.visible_text,
    };
    sess.send_event(turn_context, EventMsg::AgentMessageContentDelta(event))
        .await;
}

/// Flush buffered assistant text parser state when an assistant message item
/// ends.
pub(super) async fn flush_assistant_text_segments_for_item(
    sess: &Session,
    turn_context: &TurnContext,
    plan_mode_state: Option<&mut PlanModeStreamState>,
    parsers: &mut AssistantMessageStreamParsers,
    item_id: &str,
) {
    let parsed = parsers.finish_item(item_id);
    emit_streamed_assistant_text_delta(sess, turn_context, plan_mode_state, item_id, parsed).await;
}

/// Flush any remaining buffered assistant text parser state at response
/// completion.
pub(super) async fn flush_assistant_text_segments_all(
    sess: &Session,
    turn_context: &TurnContext,
    mut plan_mode_state: Option<&mut PlanModeStreamState>,
    parsers: &mut AssistantMessageStreamParsers,
) {
    for (item_id, parsed) in parsers.drain_finished() {
        emit_streamed_assistant_text_delta(
            sess,
            turn_context,
            plan_mode_state.as_deref_mut(),
            &item_id,
            parsed,
        )
        .await;
    }
}

/// Emit completion for plan items by parsing the finalized assistant message.
pub(super) async fn maybe_complete_plan_item_from_message(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item: &ResponseItem,
) {
    if let ResponseItem::Message { role, content, .. } = item
        && role == "assistant"
    {
        let mut text = String::new();
        for entry in content {
            if let ContentItem::OutputText { text: chunk } = entry {
                text.push_str(chunk);
            }
        }
        if let Some(plan_text) = extract_proposed_plan_text(&text) {
            let (plan_text, _citations) = strip_citations(&plan_text);
            if !state.plan_item_state.started() {
                state.plan_item_state.start(sess, turn_context).await;
            }
            state
                .plan_item_state
                .complete_with_text(sess, turn_context, plan_text)
                .await;
        }
    }
}

/// Emit a completed agent message in plan mode, respecting deferred starts.
pub(super) async fn emit_agent_message_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    agent_message: chaos_ipc::items::AgentMessageItem,
    state: &mut PlanModeStreamState,
) {
    let agent_message_id = agent_message.id.clone();
    let text = agent_message_text(&agent_message);
    if text.trim().is_empty() {
        state.pending_agent_message_items.remove(&agent_message_id);
        state.started_agent_message_items.remove(&agent_message_id);
        return;
    }

    maybe_emit_pending_agent_message_start(sess, turn_context, state, &agent_message_id).await;

    if !state
        .started_agent_message_items
        .contains(&agent_message_id)
    {
        let start_item = state
            .pending_agent_message_items
            .remove(&agent_message_id)
            .unwrap_or_else(|| {
                TurnItem::AgentMessage(chaos_ipc::items::AgentMessageItem {
                    id: agent_message_id.clone(),
                    content: Vec::new(),
                    phase: None,
                })
            });
        sess.emit_turn_item_started(turn_context, &start_item).await;
        state
            .started_agent_message_items
            .insert(agent_message_id.clone());
    }

    sess.emit_turn_item_completed(turn_context, TurnItem::AgentMessage(agent_message))
        .await;
    state.started_agent_message_items.remove(&agent_message_id);
}

/// Emit completion for a plan-mode turn item, handling agent messages
/// specially.
pub(super) async fn emit_turn_item_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    turn_item: TurnItem,
    previously_active_item: Option<&TurnItem>,
    state: &mut PlanModeStreamState,
) {
    match turn_item {
        TurnItem::AgentMessage(agent_message) => {
            emit_agent_message_in_plan_mode(sess, turn_context, agent_message, state).await;
        }
        _ => {
            if previously_active_item.is_none() {
                sess.emit_turn_item_started(turn_context, &turn_item).await;
            }
            sess.emit_turn_item_completed(turn_context, turn_item).await;
        }
    }
}

/// Handle a completed assistant response item in plan mode, returning true
/// if handled.
pub(super) async fn handle_assistant_item_done_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    item: &ResponseItem,
    state: &mut PlanModeStreamState,
    previously_active_item: Option<&TurnItem>,
    last_agent_message: &mut Option<String>,
) -> bool {
    if let ResponseItem::Message { role, .. } = item
        && role == "assistant"
    {
        maybe_complete_plan_item_from_message(sess, turn_context, state, item).await;

        if let Some(turn_item) =
            handle_non_tool_response_item(sess, turn_context, item, /*plan_mode*/ true).await
        {
            emit_turn_item_in_plan_mode(
                sess,
                turn_context,
                turn_item,
                previously_active_item,
                state,
            )
            .await;
        }

        record_completed_response_item(sess, turn_context, item).await;
        if let Some(agent_message) = last_assistant_message_from_item(item, /*plan_mode*/ true) {
            *last_agent_message = Some(agent_message);
        }
        return true;
    }
    false
}

pub(super) async fn drain_in_flight(
    in_flight: &mut FuturesOrdered<
        BoxFuture<'static, ChaosResult<chaos_ipc::models::ResponseInputItem>>,
    >,
    sess: std::sync::Arc<Session>,
    turn_context: std::sync::Arc<TurnContext>,
) -> ChaosResult<()> {
    while let Some(res) = in_flight.next().await {
        match res {
            Ok(response_input) => {
                sess.record_conversation_items(&turn_context, &[response_input.into()])
                    .await;
            }
            Err(err) => {
                error_or_panic(format!("in-flight tool future failed during drain: {err}"));
            }
        }
    }
    Ok(())
}
