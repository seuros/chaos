//! Top-level event entry points and the central dispatch table that routes
//! `EventMsg` variants to per-event handler methods.

use chaos_ipc::protocol::AgentMessageEvent;
use chaos_ipc::protocol::AgentReasoningEvent;
use chaos_ipc::protocol::AgentReasoningRawContentEvent;
use chaos_ipc::protocol::BackgroundEventEvent;
use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::CollabAgentSpawnBeginEvent;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::StreamErrorEvent;
use chaos_ipc::protocol::TurnAbortReason;
use chaos_ipc::protocol::TurnCompleteEvent;
use chaos_ipc::protocol::TurnDiffEvent;
use chaos_ipc::protocol::UserMessageEvent;
use chaos_ipc::protocol::WarningEvent;

use crate::app_event::AppEvent;
use crate::multi_agents;

use super::super::super::ChatWidget;
use super::super::super::core::RateLimitErrorKind;
use super::super::super::core::ReplayKind;
use super::super::super::core::rate_limit_error_kind;

impl ChatWidget {
    pub fn handle_codex_event(&mut self, event: Event) {
        let Event { id, msg } = event;
        self.dispatch_event_msg(Some(id), msg, /*replay_kind*/ None);
    }

    pub fn handle_codex_event_replay(&mut self, event: Event) {
        let Event { msg, .. } = event;
        if matches!(msg, EventMsg::ShutdownComplete) {
            return;
        }
        self.dispatch_event_msg(/*id*/ None, msg, Some(ReplayKind::ProcessSnapshot));
    }

    /// Dispatch a protocol `EventMsg` to the appropriate handler.
    pub(in crate::chatwidget) fn dispatch_event_msg(
        &mut self,
        id: Option<String>,
        msg: EventMsg,
        replay_kind: Option<ReplayKind>,
    ) {
        let from_replay = replay_kind.is_some();
        let is_resume_initial_replay =
            matches!(replay_kind, Some(ReplayKind::ResumeInitialMessages));
        let is_stream_error = matches!(&msg, EventMsg::StreamError(_));
        if !is_resume_initial_replay && !is_stream_error {
            self.restore_retry_status_header_if_present();
        }

        match msg {
            EventMsg::AgentMessageContentDelta(_)
            | EventMsg::PlanDelta(_)
            | EventMsg::ReasoningContentDelta(_)
            | EventMsg::ReasoningRawContentDelta(_)
            | EventMsg::TerminalInteraction(_)
            | EventMsg::ExecCommandOutputDelta(_) => {}
            _ => {
                tracing::trace!("handle_codex_event: {:?}", msg);
            }
        }

        match msg {
            EventMsg::SessionConfigured(e) => self.on_session_configured(e),
            EventMsg::ProcessNameUpdated(e) => self.on_process_name_updated(e),
            EventMsg::AgentMessage(AgentMessageEvent { .. })
                if matches!(replay_kind, Some(ReplayKind::ProcessSnapshot))
                    && !self.is_review_mode => {}
            EventMsg::AgentMessage(AgentMessageEvent { message, .. })
                if from_replay || self.is_review_mode =>
            {
                self.on_agent_message(message)
            }
            EventMsg::AgentMessage(AgentMessageEvent { .. }) => {}
            EventMsg::AgentMessageContentDelta(event) => self.on_agent_message_delta(event.delta),
            EventMsg::PlanDelta(event) => self.on_plan_delta(event.delta),
            EventMsg::ReasoningContentDelta(event) => self.on_agent_reasoning_delta(event.delta),
            EventMsg::ReasoningRawContentDelta(event) => self.on_agent_reasoning_delta(event.delta),
            EventMsg::AgentReasoning(AgentReasoningEvent { .. }) => self.on_agent_reasoning_final(),
            EventMsg::AgentReasoningRawContent(AgentReasoningRawContentEvent { text }) => {
                self.on_agent_reasoning_delta(text);
                self.on_agent_reasoning_final();
            }
            EventMsg::AgentReasoningSectionBreak(_) => self.on_reasoning_section_break(),
            EventMsg::TurnStarted(event) => {
                if !is_resume_initial_replay {
                    self.apply_turn_started_context_window(event.model_context_window);
                    self.on_task_started();
                }
            }
            EventMsg::TurnComplete(TurnCompleteEvent {
                last_agent_message, ..
            }) => self.on_task_complete(last_agent_message, from_replay),
            EventMsg::TokenCount(ev) => {
                self.set_token_info(ev.info);
                self.on_rate_limit_snapshot(ev.rate_limits);
            }
            EventMsg::Warning(WarningEvent { message }) => self.on_warning(message),
            EventMsg::ModelReroute(_) => {}
            EventMsg::Error(ErrorEvent {
                message,
                chaos_error_info,
            }) => {
                if let Some(info) = chaos_error_info {
                    if matches!(info, ChaosErrorInfo::ProviderAuthMissing { .. }) {
                        self.on_error(message);
                        self.app_event_tx.send(AppEvent::OpenLoginPopup);
                    } else if let Some(kind) = rate_limit_error_kind(&info) {
                        match kind {
                            RateLimitErrorKind::ServerOverloaded => {
                                self.on_server_overloaded_error(message)
                            }
                            RateLimitErrorKind::UsageLimit | RateLimitErrorKind::Generic => {
                                self.on_error(message)
                            }
                        }
                    } else {
                        self.on_error(message);
                    }
                } else {
                    self.on_error(message);
                }
            }
            EventMsg::McpStartupUpdate(ev) => self.on_mcp_startup_update(ev),
            EventMsg::McpStartupComplete(ev) => self.on_mcp_startup_complete(ev),
            EventMsg::TurnAborted(ev) => match ev.reason {
                TurnAbortReason::Interrupted => {
                    self.on_interrupted_turn(ev.reason);
                }
                TurnAbortReason::Replaced => {
                    self.submit_pending_steers_after_interrupt = false;
                    self.pending_steers.clear();
                    self.refresh_pending_input_preview();
                    self.on_error("Turn aborted: replaced by a new task".to_owned())
                }
                TurnAbortReason::ReviewEnded => {
                    self.on_interrupted_turn(ev.reason);
                }
            },
            EventMsg::PlanUpdate(update) => self.on_plan_update(update),
            EventMsg::ExecApprovalRequest(ev) => {
                self.on_exec_approval_request(id.unwrap_or_default(), ev)
            }
            EventMsg::ApplyPatchApprovalRequest(ev) => {
                self.on_apply_patch_approval_request(id.unwrap_or_default(), ev)
            }
            EventMsg::ElicitationRequest(ev) => {
                self.on_elicitation_request(ev);
            }
            EventMsg::ElicitationComplete(_) => {}
            EventMsg::RequestUserInput(ev) => {
                self.on_request_user_input(ev);
            }
            EventMsg::RequestPermissions(ev) => {
                self.on_request_permissions(ev);
            }
            EventMsg::ExecCommandBegin(ev) => self.on_exec_command_begin(ev),
            EventMsg::TerminalInteraction(delta) => self.on_terminal_interaction(delta),
            EventMsg::ExecCommandOutputDelta(delta) => self.on_exec_command_output_delta(delta),
            EventMsg::PatchApplyBegin(ev) => self.on_patch_apply_begin(ev),
            EventMsg::PatchApplyEnd(ev) => self.on_patch_apply_end(ev),
            EventMsg::ExecCommandEnd(ev) => self.on_exec_command_end(ev),
            EventMsg::ViewImageToolCall(ev) => self.on_view_image_tool_call(ev),
            EventMsg::ImageGenerationBegin(ev) => self.on_image_generation_begin(ev),
            EventMsg::ImageGenerationEnd(ev) => self.on_image_generation_end(ev),
            EventMsg::McpToolCallBegin(ev) => self.on_mcp_tool_call_begin(ev),
            EventMsg::McpToolCallEnd(ev) => self.on_mcp_tool_call_end(ev),
            EventMsg::WebSearchBegin(ev) => self.on_web_search_begin(ev),
            EventMsg::WebSearchEnd(ev) => self.on_web_search_end(ev),
            EventMsg::GetHistoryEntryResponse(ev) => self.on_get_history_entry_response(ev),
            EventMsg::McpListToolsResponse(ev) => self.on_list_mcp_tools(ev),
            EventMsg::AllToolsResponse(ev) => self.on_all_tools_response(ev),
            EventMsg::ListCustomPromptsResponse(ev) => self.on_list_custom_prompts(ev),
            EventMsg::ShutdownComplete => self.on_shutdown_complete(),
            EventMsg::TurnDiff(TurnDiffEvent { unified_diff }) => self.on_turn_diff(unified_diff),
            EventMsg::DeprecationNotice(ev) => self.on_deprecation_notice(ev),
            EventMsg::BackgroundEvent(BackgroundEventEvent { message }) => {
                self.on_background_event(message)
            }
            EventMsg::UndoStarted(ev) => self.on_undo_started(ev),
            EventMsg::UndoCompleted(ev) => self.on_undo_completed(ev),
            EventMsg::StreamError(StreamErrorEvent {
                message,
                additional_details,
                ..
            }) => {
                if !is_resume_initial_replay {
                    self.on_stream_error(message, additional_details);
                }
            }
            EventMsg::UserMessage(ev) => {
                if from_replay {
                    self.on_user_message_event(ev);
                }
            }
            EventMsg::EnteredReviewMode(review_request) => {
                self.on_entered_review_mode(review_request, from_replay)
            }
            EventMsg::ExitedReviewMode(review) => self.on_exited_review_mode(review),
            EventMsg::ContextCompacted(_) => self.on_agent_message("Context compacted".to_owned()),
            EventMsg::CollabAgentSpawnBegin(CollabAgentSpawnBeginEvent {
                call_id,
                model,
                reasoning_effort,
                ..
            }) => {
                self.pending_collab_spawn_requests.insert(
                    call_id,
                    multi_agents::SpawnRequestSummary {
                        model,
                        reasoning_effort,
                    },
                );
            }
            EventMsg::CollabAgentSpawnEnd(ev) => {
                let spawn_request = self.pending_collab_spawn_requests.remove(&ev.call_id);
                self.on_collab_event(multi_agents::spawn_end(ev, spawn_request.as_ref()));
            }
            EventMsg::CollabAgentInteractionBegin(_) => {}
            EventMsg::CollabAgentInteractionEnd(ev) => {
                self.on_collab_event(multi_agents::interaction_end(ev))
            }
            EventMsg::CollabWaitingBegin(ev) => {
                self.on_collab_event(multi_agents::waiting_begin(ev))
            }
            EventMsg::CollabWaitingEnd(ev) => self.on_collab_event(multi_agents::waiting_end(ev)),
            EventMsg::CollabCloseBegin(_) => {}
            EventMsg::CollabCloseEnd(ev) => self.on_collab_event(multi_agents::close_end(ev)),
            EventMsg::CollabResumeBegin(ev) => self.on_collab_event(multi_agents::resume_begin(ev)),
            EventMsg::CollabResumeEnd(ev) => self.on_collab_event(multi_agents::resume_end(ev)),
            EventMsg::ProcessRolledBack(rollback) => {
                self.last_copyable_output = None;
                if from_replay {
                    self.app_event_tx.send(AppEvent::ApplyProcessRollback {
                        num_turns: rollback.num_turns,
                    });
                }
            }
            EventMsg::RawResponseItem(_)
            | EventMsg::ItemStarted(_)
            | EventMsg::DynamicToolCallRequest(_)
            | EventMsg::DynamicToolCallResponse(_) => {}
            EventMsg::HookStarted(event) => self.on_hook_started(event),
            EventMsg::HookCompleted(event) => self.on_hook_completed(event),
            EventMsg::ItemCompleted(event) => {
                let item = event.item;
                if let chaos_ipc::items::TurnItem::UserMessage(item) = &item {
                    let event = item.to_user_message_event();
                    let rendered = Self::rendered_user_message_event_from_event(&event);
                    if from_replay {
                        if self.last_rendered_user_message_event.as_ref() != Some(&rendered) {
                            self.on_user_message_event(event);
                        }
                    } else {
                        let compare_key = Self::pending_steer_compare_key_from_item(item);
                        if self
                            .pending_steers
                            .front()
                            .is_some_and(|pending| pending.compare_key == compare_key)
                        {
                            if let Some(pending) = self.pending_steers.pop_front() {
                                self.refresh_pending_input_preview();
                                let pending_event = UserMessageEvent {
                                    message: pending.user_message.text,
                                    images: Some(pending.user_message.remote_image_urls),
                                    local_images: pending
                                        .user_message
                                        .local_images
                                        .into_iter()
                                        .map(|image| image.path)
                                        .collect(),
                                    text_elements: pending.user_message.text_elements,
                                };
                                self.on_user_message_event(pending_event);
                            } else if self.last_rendered_user_message_event.as_ref()
                                != Some(&rendered)
                            {
                                tracing::warn!(
                                    "pending steer matched compare key but queue was empty when rendering committed user message"
                                );
                                self.on_user_message_event(event);
                            }
                        } else if self.last_rendered_user_message_event.as_ref() != Some(&rendered)
                        {
                            self.on_user_message_event(event);
                        }
                    }
                }
                if let chaos_ipc::items::TurnItem::Plan(plan_item) = &item {
                    self.on_plan_item_completed(plan_item.text.clone());
                }
                if let chaos_ipc::items::TurnItem::AgentMessage(item) = item {
                    self.on_agent_message_item_completed(item);
                }
            }
        }

        if !from_replay && self.agent_turn_running {
            self.refresh_runtime_metrics();
        }
    }
}
