use std::sync::Arc;

use chaos_epoll::OrCancelExt;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::protocol::EventMsg;
use futures::future::BoxFuture;
use futures::prelude::*;
use futures::stream::FuturesOrdered;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::field;
use tracing::instrument;
use tracing::trace_span;
use tracing::warn;

use crate::client::ModelClientSession;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::error::ChaosErr;
use crate::error::Result as ChaosResult;
use crate::stream_events_utils::HandleOutputCtx;
use crate::stream_events_utils::handle_non_tool_response_item;
use crate::stream_events_utils::handle_output_item_done;
use crate::stream_events_utils::raw_assistant_output_text_from_item;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::parallel::ToolCallRuntime;
use crate::turn_timing::record_turn_ttft_metric;

use super::super::Session;
use super::super::TurnContext;
use super::super::response_parsing::{
    AssistantMessageStreamParsers, ParsedAssistantTextDelta, PlanModeStreamState, drain_in_flight,
    emit_streamed_assistant_text_delta, flush_assistant_text_segments_all,
    flush_assistant_text_segments_for_item, handle_assistant_item_done_in_plan_mode,
};
use super::SamplingRequestResult;

#[allow(clippy::too_many_arguments)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %turn_context.model_info.slug
    )
)]
pub(super) async fn try_run_sampling_request(
    tool_runtime: ToolCallRuntime,
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    client_session: &mut ModelClientSession,
    turn_metadata_header: Option<&str>,
    turn_diff_tracker: SharedTurnDiffTracker,
    server_model_warning_emitted_for_turn: &mut bool,
    last_server_model: &mut Option<String>,
    prompt: &Prompt,
    cancellation_token: CancellationToken,
) -> ChaosResult<SamplingRequestResult> {
    crate::feedback_tags!(
        model = turn_context.model_info.slug.clone(),
        approval_policy = turn_context.approval_policy.value(),
        sandbox_policy = crate::sandbox_tags::sandbox_policy_tag_for_policies(
            &turn_context.vfs_policy,
            turn_context.socket_policy,
            &turn_context.cwd,
        ),
        effort = turn_context.reasoning_effort,
        auth_mode = sess.services.auth_manager.auth_mode(),
        features = sess.features.enabled_features(),
    );
    let mut stream = client_session
        .stream(
            prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort,
            turn_context.reasoning_summary,
            turn_context.config.service_tier,
            turn_metadata_header,
        )
        .instrument(trace_span!("stream_request"))
        .or_cancel(&cancellation_token)
        .await??;
    let mut in_flight: FuturesOrdered<BoxFuture<'static, ChaosResult<ResponseInputItem>>> =
        FuturesOrdered::new();
    let mut needs_follow_up = false;
    let mut last_agent_message: Option<String> = None;
    let mut active_item: Option<TurnItem> = None;
    let mut should_emit_turn_diff = false;
    let plan_mode = turn_context.collaboration_mode.mode == ModeKind::Plan;
    let mut assistant_message_stream_parsers = AssistantMessageStreamParsers::new(plan_mode);
    let mut plan_mode_state = plan_mode.then(|| PlanModeStreamState::new(&turn_context.sub_id));
    let receiving_span = trace_span!("receiving_stream");
    let outcome: ChaosResult<SamplingRequestResult> = loop {
        let handle_responses = trace_span!(
            parent: &receiving_span,
            "handle_responses",
            otel.name = field::Empty,
            tool_name = field::Empty,
            from = field::Empty,
        );

        let event = match stream
            .next()
            .instrument(trace_span!(parent: &handle_responses, "receiving"))
            .or_cancel(&cancellation_token)
            .await
        {
            Ok(event) => event,
            Err(chaos_epoll::CancelErr::Cancelled) => break Err(ChaosErr::TurnAborted),
        };

        let event = match event {
            Some(res) => res?,
            None => {
                break Err(ChaosErr::Stream(
                    "stream closed before response.completed".into(),
                    None,
                ));
            }
        };

        sess.services
            .session_telemetry
            .record_responses(&handle_responses, &event);
        record_turn_ttft_metric(&turn_context, &event).await;

        match event {
            ResponseEvent::Created => {}
            ResponseEvent::OutputItemDone(item) => {
                let previously_active_item = active_item.take();
                if let Some(previous) = previously_active_item.as_ref()
                    && matches!(previous, TurnItem::AgentMessage(_))
                {
                    let item_id = previous.id();
                    flush_assistant_text_segments_for_item(
                        &sess,
                        &turn_context,
                        plan_mode_state.as_mut(),
                        &mut assistant_message_stream_parsers,
                        &item_id,
                    )
                    .await;
                }
                if let Some(state) = plan_mode_state.as_mut()
                    && handle_assistant_item_done_in_plan_mode(
                        &sess,
                        &turn_context,
                        &item,
                        state,
                        previously_active_item.as_ref(),
                        &mut last_agent_message,
                    )
                    .await
                {
                    continue;
                }

                let mut ctx = HandleOutputCtx {
                    sess: sess.clone(),
                    turn_context: turn_context.clone(),
                    tool_runtime: tool_runtime.clone(),
                    cancellation_token: cancellation_token.child_token(),
                };

                let output_result = handle_output_item_done(&mut ctx, item, previously_active_item)
                    .instrument(handle_responses)
                    .await?;
                if let Some(tool_future) = output_result.tool_future {
                    in_flight.push_back(tool_future);
                }
                if let Some(agent_message) = output_result.last_agent_message {
                    last_agent_message = Some(agent_message);
                }
                needs_follow_up |= output_result.needs_follow_up;
            }
            ResponseEvent::OutputItemAdded(item) => {
                if let Some(turn_item) = handle_non_tool_response_item(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &item,
                    plan_mode,
                )
                .await
                {
                    let mut turn_item = turn_item;
                    let mut seeded_parsed: Option<ParsedAssistantTextDelta> = None;
                    let mut seeded_item_id: Option<String> = None;
                    if matches!(turn_item, TurnItem::AgentMessage(_))
                        && let Some(raw_text) = raw_assistant_output_text_from_item(&item)
                    {
                        let item_id = turn_item.id();
                        let mut seeded =
                            assistant_message_stream_parsers.seed_item_text(&item_id, &raw_text);
                        if let TurnItem::AgentMessage(agent_message) = &mut turn_item {
                            agent_message.content =
                                vec![chaos_ipc::items::AgentMessageContent::Text {
                                    text: if plan_mode {
                                        String::new()
                                    } else {
                                        std::mem::take(&mut seeded.visible_text)
                                    },
                                }];
                        }
                        seeded_parsed = plan_mode.then_some(seeded);
                        seeded_item_id = Some(item_id);
                    }
                    if let Some(state) = plan_mode_state.as_mut()
                        && matches!(turn_item, TurnItem::AgentMessage(_))
                    {
                        let item_id = turn_item.id();
                        state
                            .pending_agent_message_items
                            .insert(item_id, turn_item.clone());
                    } else {
                        sess.emit_turn_item_started(&turn_context, &turn_item).await;
                    }
                    if let (Some(state), Some(item_id), Some(parsed)) = (
                        plan_mode_state.as_mut(),
                        seeded_item_id.as_deref(),
                        seeded_parsed,
                    ) {
                        emit_streamed_assistant_text_delta(
                            &sess,
                            &turn_context,
                            Some(state),
                            item_id,
                            parsed,
                        )
                        .await;
                    }
                    active_item = Some(turn_item);
                }
            }
            ResponseEvent::ServerModel(server_model) => {
                *last_server_model = Some(server_model.clone());
                if !*server_model_warning_emitted_for_turn
                    && sess
                        .maybe_warn_on_server_model_mismatch(&turn_context, server_model)
                        .await
                {
                    *server_model_warning_emitted_for_turn = true;
                }
            }
            ResponseEvent::ServerReasoningIncluded(included) => {
                sess.set_server_reasoning_included(included).await;
            }
            ResponseEvent::RateLimits(snapshot) => {
                // Update internal state with latest rate limits, but defer sending until
                // token usage is available to avoid duplicate TokenCount events.
                sess.update_rate_limits(&turn_context, snapshot).await;
            }
            ResponseEvent::ModelsEtag(etag) => {
                // Update internal state with latest models etag
                sess.services.models_manager.refresh_if_new_etag(etag).await;
            }
            ResponseEvent::Completed {
                response_id,
                token_usage,
            } => {
                if let Some(usage) = &token_usage {
                    let model_name = last_server_model
                        .as_deref()
                        .unwrap_or(turn_context.model_info.slug.as_str());
                    tracing::info!(
                        provider = turn_context.provider.name.as_str(),
                        model = model_name,
                        response_id = %response_id,
                        input_tokens = usage.input_tokens,
                        cached_input_tokens = usage.cached_input_tokens,
                        output_tokens = usage.output_tokens,
                        reasoning_output_tokens = usage.reasoning_output_tokens,
                        total_tokens = usage.total_tokens,
                        "ration: turn completed",
                    );
                }
                flush_assistant_text_segments_all(
                    &sess,
                    &turn_context,
                    plan_mode_state.as_mut(),
                    &mut assistant_message_stream_parsers,
                )
                .await;
                sess.update_token_usage_info(&turn_context, token_usage.as_ref())
                    .await;
                should_emit_turn_diff = true;

                // Use the phase-aware check: pending input that was
                // deferred to the next turn does not extend the current
                // turn even if it exists in the mailbox.
                needs_follow_up |= sess.has_deliverable_input().await;

                break Ok(SamplingRequestResult {
                    needs_follow_up,
                    last_agent_message,
                });
            }
            ResponseEvent::OutputTextDelta(delta) => {
                // In review child threads, suppress assistant text deltas; the
                // UI will show a selection popup from the final ReviewOutput.
                //
                // Some providers (xAI) stream output_text.delta events for a
                // new text segment without a preceding output_item.added when
                // multiple output items are interleaved. Synthesize a fallback
                // AgentMessage so these deltas are not silently dropped.
                if active_item.is_none() {
                    warn!(
                        "OutputTextDelta arrived with no active item — \
                         synthesizing fallback AgentMessage (provider sent \
                         interleaved output items without output_item.added)"
                    );
                    let fallback = TurnItem::AgentMessage(chaos_ipc::items::AgentMessageItem {
                        id: uuid::Uuid::new_v4().to_string(),
                        content: vec![],
                        phase: None,
                    });
                    sess.emit_turn_item_started(&turn_context, &fallback).await;
                    active_item = Some(fallback);
                }
                if let Some(active) = active_item.as_ref() {
                    let item_id = active.id();
                    if matches!(active, TurnItem::AgentMessage(_)) {
                        let parsed = assistant_message_stream_parsers.parse_delta(&item_id, &delta);
                        emit_streamed_assistant_text_delta(
                            &sess,
                            &turn_context,
                            plan_mode_state.as_mut(),
                            &item_id,
                            parsed,
                        )
                        .await;
                    } else {
                        let event = crate::protocol::AgentMessageContentDeltaEvent {
                            process_id: sess.conversation_id.to_string(),
                            turn_id: turn_context.sub_id.clone(),
                            item_id,
                            delta,
                        };
                        sess.send_event(&turn_context, EventMsg::AgentMessageContentDelta(event))
                            .await;
                    }
                }
            }
            ResponseEvent::ReasoningSummaryDelta {
                delta,
                summary_index,
            } => {
                if let Some(active) = active_item.as_ref() {
                    let event = crate::protocol::ReasoningContentDeltaEvent {
                        process_id: sess.conversation_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        summary_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningContentDelta(event))
                        .await;
                } else {
                    crate::util::error_or_panic(
                        "ReasoningSummaryDelta without active item".to_string(),
                    );
                }
            }
            ResponseEvent::ReasoningSummaryPartAdded { summary_index } => {
                if let Some(active) = active_item.as_ref() {
                    let event = EventMsg::AgentReasoningSectionBreak(
                        crate::protocol::AgentReasoningSectionBreakEvent {
                            item_id: active.id(),
                            summary_index,
                        },
                    );
                    sess.send_event(&turn_context, event).await;
                } else {
                    crate::util::error_or_panic(
                        "ReasoningSummaryPartAdded without active item".to_string(),
                    );
                }
            }
            ResponseEvent::ReasoningContentDelta {
                delta,
                content_index,
            } => {
                if let Some(active) = active_item.as_ref() {
                    let event = crate::protocol::ReasoningRawContentDeltaEvent {
                        process_id: sess.conversation_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        content_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningRawContentDelta(event))
                        .await;
                } else {
                    crate::util::error_or_panic(
                        "ReasoningRawContentDelta without active item".to_string(),
                    );
                }
            }
        }
    };

    flush_assistant_text_segments_all(
        &sess,
        &turn_context,
        plan_mode_state.as_mut(),
        &mut assistant_message_stream_parsers,
    )
    .await;

    drain_in_flight(&mut in_flight, sess.clone(), turn_context.clone()).await?;

    if cancellation_token.is_cancelled() {
        return Err(ChaosErr::TurnAborted);
    }

    if should_emit_turn_diff {
        let unified_diff = {
            let mut tracker = turn_diff_tracker.lock().await;
            tracker.get_unified_diff()
        };
        if let Ok(Some(unified_diff)) = unified_diff {
            let msg = EventMsg::TurnDiff(crate::protocol::TurnDiffEvent { unified_diff });
            sess.clone().send_event(&turn_context, msg).await;
        }
    }

    outcome
}
