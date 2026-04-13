//! Protocol event dispatch for `ChatWidget`.
//!
//! This module collects the methods that consume `EventMsg` values from the
//! chaos-kern event stream and translate them into widget state mutations.
//! The public surface is `handle_codex_event` and `handle_codex_event_replay`
//! which route through the private `dispatch_event_msg` dispatcher.

use chaos_ipc::approvals::ElicitationRequestEvent;
use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::protocol::AgentMessageEvent;
use chaos_ipc::protocol::AgentReasoningEvent;
use chaos_ipc::protocol::AgentReasoningRawContentEvent;
use chaos_ipc::protocol::ApplyPatchApprovalRequestEvent;
use chaos_ipc::protocol::BackgroundEventEvent;
use chaos_ipc::protocol::CollabAgentSpawnBeginEvent;
use chaos_ipc::protocol::DeprecationNoticeEvent;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecApprovalRequestEvent;
use chaos_ipc::protocol::ExecCommandBeginEvent;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::ExecCommandOutputDeltaEvent;
use chaos_ipc::protocol::ExecCommandSource;
use chaos_ipc::protocol::ExitedReviewModeEvent;
use chaos_ipc::protocol::ImageGenerationBeginEvent;
use chaos_ipc::protocol::ImageGenerationEndEvent;
use chaos_ipc::protocol::McpStartupCompleteEvent;
use chaos_ipc::protocol::McpStartupStatus;
use chaos_ipc::protocol::McpStartupUpdateEvent;
use chaos_ipc::protocol::McpToolCallBeginEvent;
use chaos_ipc::protocol::McpToolCallEndEvent;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::PatchApplyBeginEvent;
use chaos_ipc::protocol::ReviewRequest;
use chaos_ipc::protocol::StreamErrorEvent;
use chaos_ipc::protocol::TerminalInteractionEvent;
use chaos_ipc::protocol::TurnAbortReason;
use chaos_ipc::protocol::TurnCompleteEvent;
use chaos_ipc::protocol::TurnDiffEvent;
use chaos_ipc::protocol::UndoCompletedEvent;
use chaos_ipc::protocol::UndoStartedEvent;
use chaos_ipc::protocol::UserMessageEvent;
use chaos_ipc::protocol::ViewImageToolCallEvent;
use chaos_ipc::protocol::WarningEvent;
use chaos_ipc::protocol::WebSearchBeginEvent;
use chaos_ipc::protocol::WebSearchEndEvent;
use chaos_ipc::request_permissions::RequestPermissionsEvent;
use chaos_ipc::request_user_input::RequestUserInputEvent;
use chaos_ipc::user_input::TextElement;
use chaos_ipc::user_input::UserInput;
use chaos_kern::config::Constrained;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use std::path::PathBuf;
use std::time::Instant;

use crate::app_event::AppEvent;
use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::McpServerElicitationFormRequest;
use crate::bottom_pane::QUIT_SHORTCUT_TIMEOUT;
use crate::bottom_pane::SelectionViewParams;
use crate::clipboard_paste::paste_image_to_temp_png;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::ExecCell;
use crate::exec_cell::new_active_exec_command;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::McpToolCallCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::WebSearchCell;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::markdown::append_markdown;
use crate::multi_agents;
use crate::status_indicator_widget::StatusDetailsCapitalization;
use crate::tui::FrameRequester;

use super::ChatWidget;
use super::ExternalEditorState;
use super::UserMessage;
use super::core::PendingSteerCompareKey;
use super::core::RateLimitErrorKind;
use super::core::RenderedUserMessageEvent;
use super::core::ReplayKind;
use super::core::RunningCommand;
use super::core::UnifiedExecProcessSummary;
use super::core::UnifiedExecWaitState;
use super::core::UnifiedExecWaitStreak;
#[cfg(test)]
use super::core::append_text_with_rebased_elements;
use super::core::hook_event_label;
use super::core::is_standard_tool_call;
use super::core::is_unified_exec_source;
use super::core::merge_user_messages;
use super::core::rate_limit_error_kind;

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
    fn dispatch_event_msg(
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
                if let Some(info) = chaos_error_info
                    && let Some(kind) = rate_limit_error_kind(&info)
                {
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
            EventMsg::ListSkillsResponse(_) => {}
            EventMsg::ListRemoteSkillsResponse(_) | EventMsg::RemoteSkillDownloaded(_) => {}
            EventMsg::SkillsUpdateAvailable => {}
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

    // ── Session / process events ──────────────────────────────────────────────

    pub(super) fn on_session_configured(
        &mut self,
        event: chaos_ipc::protocol::SessionConfiguredEvent,
    ) {
        self.bottom_pane
            .set_history_metadata(event.history_log_id, event.history_entry_count);
        self.session_network_proxy = event.network_proxy.clone();
        self.process_id = Some(event.session_id);
        self.process_name = event.process_name.clone();
        self.forked_from = event.forked_from_id;
        self.current_cwd = Some(event.cwd.clone());
        self.config.cwd = event.cwd.clone();
        if let Err(err) = self
            .config
            .permissions
            .approval_policy
            .set(event.approval_policy)
        {
            tracing::warn!(%err, "failed to sync approval_policy from SessionConfigured");
            self.config.permissions.approval_policy =
                Constrained::allow_only(event.approval_policy);
        }
        if let Err(err) = self
            .config
            .permissions
            .sandbox_policy
            .set(event.sandbox_policy.clone())
        {
            tracing::warn!(%err, "failed to sync sandbox_policy from SessionConfigured");
            self.config.permissions.sandbox_policy =
                Constrained::allow_only(event.sandbox_policy.clone());
        }
        self.config.approvals_reviewer = event.approvals_reviewer;
        let initial_messages = event.initial_messages.clone();
        self.last_copyable_output = None;
        let forked_from_id = event.forked_from_id;
        let model_for_header = event.model.clone();
        self.session_header.set_model(&model_for_header);
        self.current_collaboration_mode = self.current_collaboration_mode.with_updates(
            Some(model_for_header.clone()),
            Some(event.reasoning_effort),
            /*minion_instructions*/ None,
        );
        if let Some(mask) = self.active_collaboration_mask.as_mut() {
            mask.model = Some(model_for_header.clone());
            mask.reasoning_effort = Some(event.reasoning_effort);
        }
        self.refresh_model_display();
        self.sync_personality_command_enabled();
        let session_info_cell = history_cell::new_session_info(
            &self.config,
            &model_for_header,
            event,
            self.show_welcome_banner,
        );
        self.apply_session_info_cell(session_info_cell);

        if let Some(messages) = initial_messages {
            self.replay_initial_messages(messages);
        }
        self.submit_op(chaos_ipc::protocol::Op::ListCustomPrompts);
        if self.connectors_enabled() {
            self.prefetch_connectors();
        }
        if let Some(user_message) = self.initial_user_message.take() {
            self.submit_user_message(user_message);
        }
        if let Some(forked_from_id) = forked_from_id {
            self.emit_forked_process_event(forked_from_id);
        }
        if !self.suppress_session_configured_redraw {
            self.request_redraw();
        }
    }

    pub(super) fn on_process_name_updated(
        &mut self,
        event: chaos_ipc::protocol::ProcessNameUpdatedEvent,
    ) {
        if self.process_id == Some(event.process_id) {
            self.process_name = event.process_name;
            self.request_redraw();
        }
    }

    // ── Turn lifecycle events ─────────────────────────────────────────────────

    pub(super) fn on_task_started(&mut self) {
        self.agent_turn_running = true;
        self.turn_sleep_inhibitor
            .set_turn_running(/*turn_running*/ true);
        self.saw_plan_update_this_turn = false;
        self.saw_plan_item_this_turn = false;
        self.plan_delta_buffer.clear();
        self.plan_item_active = false;
        self.adaptive_chunking.reset();
        self.plan_stream_controller = None;
        self.turn_runtime_metrics = chaos_syslog::RuntimeMetricsSummary::default();
        self.session_telemetry.reset_runtime_metrics();
        self.bottom_pane.clear_quit_shortcut_hint();
        self.quit_shortcut_expires_at = None;
        self.quit_shortcut_key = None;
        self.update_task_running_state();
        self.retry_status_header = None;
        self.pending_status_indicator_restore = false;
        self.bottom_pane
            .set_interrupt_hint_visible(/*visible*/ true);
        self.set_status_header(String::from("Working"));
        self.full_reasoning_buffer.clear();
        self.reasoning_buffer.clear();
        self.request_redraw();
    }

    pub(super) fn on_task_complete(
        &mut self,
        last_agent_message: Option<String>,
        from_replay: bool,
    ) {
        use super::core::Notification;
        self.submit_pending_steers_after_interrupt = false;
        if let Some(message) = last_agent_message.as_ref()
            && !message.trim().is_empty()
        {
            self.last_copyable_output = Some(message.clone());
        }
        self.flush_answer_stream_with_separator();
        if let Some(mut controller) = self.plan_stream_controller.take()
            && let Some(cell) = controller.finalize()
        {
            self.add_boxed_history(cell);
        }
        self.flush_unified_exec_wait_streak();
        if !from_replay {
            self.collect_runtime_metrics_delta();
            let runtime_metrics =
                (!self.turn_runtime_metrics.is_empty()).then_some(self.turn_runtime_metrics);
            let show_work_separator = self.needs_final_message_separator && self.had_work_activity;
            if show_work_separator || runtime_metrics.is_some() {
                let elapsed_seconds = if show_work_separator {
                    self.bottom_pane
                        .status_widget()
                        .map(crate::status_indicator_widget::StatusIndicatorWidget::elapsed_seconds)
                        .map(|current| self.worked_elapsed_from(current))
                } else {
                    None
                };
                self.add_to_history(history_cell::FinalMessageSeparator::new(
                    elapsed_seconds,
                    runtime_metrics,
                ));
            }
            self.turn_runtime_metrics = chaos_syslog::RuntimeMetricsSummary::default();
            self.needs_final_message_separator = false;
            self.had_work_activity = false;
            self.request_status_line_branch_refresh();
        }
        self.pending_status_indicator_restore = false;
        self.agent_turn_running = false;
        self.turn_sleep_inhibitor
            .set_turn_running(/*turn_running*/ false);
        self.update_task_running_state();
        self.running_commands.clear();
        self.suppressed_exec_calls.clear();
        self.last_unified_wait = None;
        self.unified_exec_wait_streak = None;
        self.request_redraw();

        let had_pending_steers = !self.pending_steers.is_empty();
        self.refresh_pending_input_preview();

        if !from_replay && self.queued_user_messages.is_empty() && !had_pending_steers {
            self.maybe_prompt_plan_implementation();
        }
        if !from_replay {
            self.saw_plan_item_this_turn = false;
        }
        self.maybe_send_next_queued_input();
        self.notify(Notification::AgentTurnComplete {
            response: last_agent_message.unwrap_or_default(),
        });

        self.maybe_show_pending_rate_limit_prompt();
    }

    pub(super) fn finalize_turn(&mut self) {
        self.finalize_active_cell_as_failed();
        self.agent_turn_running = false;
        self.turn_sleep_inhibitor
            .set_turn_running(/*turn_running*/ false);
        self.update_task_running_state();
        self.running_commands.clear();
        self.suppressed_exec_calls.clear();
        self.last_unified_wait = None;
        self.unified_exec_wait_streak = None;
        self.adaptive_chunking.reset();
        self.stream_controller = None;
        self.plan_stream_controller = None;
        self.pending_status_indicator_restore = false;
        self.request_status_line_branch_refresh();
        self.maybe_show_pending_rate_limit_prompt();
    }

    pub(super) fn on_server_overloaded_error(&mut self, message: String) {
        self.submit_pending_steers_after_interrupt = false;
        self.finalize_turn();

        let message = if message.trim().is_empty() {
            "Chaos is currently experiencing high load.".to_string()
        } else {
            message
        };

        self.add_to_history(history_cell::new_warning_event(message));
        self.request_redraw();
        self.maybe_send_next_queued_input();
    }

    pub(super) fn on_error(&mut self, message: String) {
        self.submit_pending_steers_after_interrupt = false;
        self.finalize_turn();
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();
        self.maybe_send_next_queued_input();
    }

    pub(super) fn on_warning(&mut self, message: impl Into<String>) {
        self.add_to_history(history_cell::new_warning_event(message.into()));
        self.request_redraw();
    }

    // ── MCP startup events ────────────────────────────────────────────────────

    pub(super) fn on_mcp_startup_update(&mut self, ev: McpStartupUpdateEvent) {
        let mut status = self.mcp_startup_status.take().unwrap_or_default();
        if let McpStartupStatus::Failed { error } = &ev.status {
            self.on_warning(error);
        }
        status.insert(ev.server, ev.status);
        self.mcp_startup_status = Some(status);
        self.update_task_running_state();
        if let Some(current) = &self.mcp_startup_status {
            let total = current.len();
            let mut starting: Vec<_> = current
                .iter()
                .filter_map(|(name, state)| {
                    if matches!(state, McpStartupStatus::Starting) {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect();
            starting.sort();
            if let Some(first) = starting.first() {
                let completed = total.saturating_sub(starting.len());
                let max_to_show = 3;
                let mut to_show: Vec<String> = starting
                    .iter()
                    .take(max_to_show)
                    .map(ToString::to_string)
                    .collect();
                if starting.len() > max_to_show {
                    to_show.push("…".to_string());
                }
                let header = if total > 1 {
                    format!(
                        "Starting MCP servers ({completed}/{total}): {}",
                        to_show.join(", ")
                    )
                } else {
                    format!("Booting MCP server: {first}")
                };
                self.set_status_header(header);
            }
        }
        self.request_redraw();
    }

    pub(super) fn on_mcp_startup_complete(&mut self, ev: McpStartupCompleteEvent) {
        let mut parts = Vec::new();
        if !ev.failed.is_empty() {
            let failed_servers: Vec<_> = ev.failed.iter().map(|f| f.server.clone()).collect();
            parts.push(format!("failed: {}", failed_servers.join(", ")));
        }
        if !ev.cancelled.is_empty() {
            self.on_warning(format!(
                "MCP startup interrupted. The following servers were not initialized: {}",
                ev.cancelled.join(", ")
            ));
        }
        if !parts.is_empty() {
            self.on_warning(format!("MCP startup incomplete ({})", parts.join("; ")));
        }

        self.mcp_startup_status = None;
        self.update_task_running_state();
        self.maybe_send_next_queued_input();
        self.request_redraw();
    }

    // ── Interrupt / abort events ──────────────────────────────────────────────

    pub(super) fn on_interrupted_turn(&mut self, reason: TurnAbortReason) {
        self.finalize_turn();
        let send_pending_steers_immediately = self.submit_pending_steers_after_interrupt;
        self.submit_pending_steers_after_interrupt = false;
        if reason != TurnAbortReason::ReviewEnded {
            if send_pending_steers_immediately {
                self.add_to_history(history_cell::new_info_event(
                    "Model interrupted to submit steer instructions.".to_owned(),
                    /*hint*/ None,
                ));
            } else {
                self.add_to_history(history_cell::new_error_event(
                    "Conversation interrupted - tell the model what to do differently.".to_owned(),
                ));
            }
        }

        if send_pending_steers_immediately {
            let pending_steers: Vec<super::core::UserMessage> = self
                .pending_steers
                .drain(..)
                .map(|pending| pending.user_message)
                .collect();
            if !pending_steers.is_empty() {
                self.submit_user_message(merge_user_messages(pending_steers));
            } else if let Some(combined) = self.drain_pending_messages_for_restore() {
                self.restore_user_message_to_composer(combined);
            }
        } else if let Some(combined) = self.drain_pending_messages_for_restore() {
            self.restore_user_message_to_composer(combined);
        }
        self.refresh_pending_input_preview();

        self.request_redraw();
    }

    // ── Exec events ───────────────────────────────────────────────────────────

    pub(super) fn on_exec_approval_request(&mut self, _id: String, ev: ExecApprovalRequestEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_exec_approval(ev),
            |s| s.handle_exec_approval_now(ev2),
        );
    }

    pub(super) fn on_apply_patch_approval_request(
        &mut self,
        _id: String,
        ev: ApplyPatchApprovalRequestEvent,
    ) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_apply_patch_approval(ev),
            |s| s.handle_apply_patch_approval_now(ev2),
        );
    }

    pub(super) fn on_elicitation_request(&mut self, ev: ElicitationRequestEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_elicitation(ev),
            |s| s.handle_elicitation_request_now(ev2),
        );
    }

    pub(super) fn on_request_user_input(&mut self, ev: RequestUserInputEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_user_input(ev),
            |s| s.handle_request_user_input_now(ev2),
        );
    }

    pub(super) fn on_request_permissions(&mut self, ev: RequestPermissionsEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_request_permissions(ev),
            |s| s.handle_request_permissions_now(ev2),
        );
    }

    pub(super) fn on_exec_command_begin(&mut self, ev: ExecCommandBeginEvent) {
        self.flush_answer_stream_with_separator();
        if is_unified_exec_source(ev.source) {
            self.track_unified_exec_process_begin(&ev);
            if !self.bottom_pane.is_task_running() {
                return;
            }
            self.bottom_pane.ensure_status_indicator();
            if !is_standard_tool_call(&ev.parsed_cmd) {
                return;
            }
        }
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_exec_begin(ev), |s| s.handle_exec_begin_now(ev2));
    }

    pub(super) fn on_exec_command_output_delta(&mut self, ev: ExecCommandOutputDeltaEvent) {
        self.track_unified_exec_output_chunk(&ev.call_id, &ev.chunk);
        if !self.bottom_pane.is_task_running() {
            return;
        }

        let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|c| c.as_any_mut().downcast_mut::<ExecCell>())
        else {
            return;
        };

        if cell.append_output(&ev.call_id, std::str::from_utf8(&ev.chunk).unwrap_or("")) {
            self.bump_active_cell_revision();
            self.request_redraw();
        }
    }

    pub(super) fn on_terminal_interaction(&mut self, ev: TerminalInteractionEvent) {
        if !self.bottom_pane.is_task_running() {
            return;
        }
        self.flush_answer_stream_with_separator();
        let command_display = self
            .unified_exec_processes
            .iter()
            .find(|process| process.key == ev.process_id)
            .map(|process| process.command_display.clone());
        if ev.stdin.is_empty() {
            self.bottom_pane.ensure_status_indicator();
            self.bottom_pane
                .set_interrupt_hint_visible(/*visible*/ true);
            self.set_status(
                "Waiting for background terminal".to_string(),
                command_display.clone(),
                StatusDetailsCapitalization::Preserve,
                /*details_max_lines*/ 1,
            );
            match &mut self.unified_exec_wait_streak {
                Some(wait) if wait.process_id == ev.process_id => {
                    wait.update_command_display(command_display);
                }
                Some(_) => {
                    self.flush_unified_exec_wait_streak();
                    self.unified_exec_wait_streak =
                        Some(UnifiedExecWaitStreak::new(ev.process_id, command_display));
                }
                None => {
                    self.unified_exec_wait_streak =
                        Some(UnifiedExecWaitStreak::new(ev.process_id, command_display));
                }
            }
            self.request_redraw();
        } else {
            if self
                .unified_exec_wait_streak
                .as_ref()
                .is_some_and(|wait| wait.process_id == ev.process_id)
            {
                self.flush_unified_exec_wait_streak();
            }
            self.add_to_history(history_cell::new_unified_exec_interaction(
                command_display,
                ev.stdin,
            ));
        }
    }

    pub(super) fn on_patch_apply_begin(&mut self, event: PatchApplyBeginEvent) {
        self.add_to_history(history_cell::new_patch_event(
            event.changes,
            &self.config.cwd,
        ));
    }

    pub(super) fn on_view_image_tool_call(&mut self, event: ViewImageToolCallEvent) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_view_image_tool_call(
            event.path,
            &self.config.cwd,
        ));
        self.request_redraw();
    }

    pub(super) fn on_image_generation_begin(&mut self, _event: ImageGenerationBeginEvent) {
        self.flush_answer_stream_with_separator();
    }

    pub(super) fn on_image_generation_end(&mut self, event: ImageGenerationEndEvent) {
        self.flush_answer_stream_with_separator();
        let saved_to = event.saved_path.as_deref().and_then(|saved_path| {
            std::path::Path::new(saved_path)
                .parent()
                .map(|parent| parent.display().to_string())
        });
        self.add_to_history(history_cell::new_image_generation_call(
            event.call_id,
            event.revised_prompt,
            saved_to,
        ));
        self.request_redraw();
    }

    pub(super) fn on_patch_apply_end(&mut self, event: chaos_ipc::protocol::PatchApplyEndEvent) {
        let ev2 = event.clone();
        self.defer_or_handle(
            |q| q.push_patch_end(event),
            |s| s.handle_patch_apply_end_now(ev2),
        );
    }

    pub(super) fn on_exec_command_end(&mut self, ev: ExecCommandEndEvent) {
        if is_unified_exec_source(ev.source) {
            if let Some(process_id) = ev.process_id.as_deref()
                && self
                    .unified_exec_wait_streak
                    .as_ref()
                    .is_some_and(|wait| wait.process_id == process_id)
            {
                self.flush_unified_exec_wait_streak();
            }
            self.track_unified_exec_process_end(&ev);
            if !self.bottom_pane.is_task_running() {
                return;
            }
        }
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_exec_end(ev), |s| s.handle_exec_end_now(ev2));
    }

    pub(super) fn track_unified_exec_process_begin(&mut self, ev: &ExecCommandBeginEvent) {
        if ev.source != ExecCommandSource::UnifiedExecStartup {
            return;
        }
        let key = ev.process_id.clone().unwrap_or(ev.call_id.to_string());
        let command_display = strip_bash_lc_and_escape(&ev.command);
        if let Some(existing) = self
            .unified_exec_processes
            .iter_mut()
            .find(|process| process.key == key)
        {
            existing.call_id = ev.call_id.clone();
            existing.command_display = command_display;
            existing.recent_chunks.clear();
        } else {
            self.unified_exec_processes.push(UnifiedExecProcessSummary {
                key,
                call_id: ev.call_id.clone(),
                command_display,
                recent_chunks: Vec::new(),
            });
        }
        self.sync_unified_exec_footer();
    }

    pub(super) fn track_unified_exec_process_end(&mut self, ev: &ExecCommandEndEvent) {
        let key = ev.process_id.clone().unwrap_or(ev.call_id.to_string());
        let before = self.unified_exec_processes.len();
        self.unified_exec_processes
            .retain(|process| process.key != key);
        if self.unified_exec_processes.len() != before {
            self.sync_unified_exec_footer();
        }
    }

    pub(super) fn sync_unified_exec_footer(&mut self) {
        let processes = self
            .unified_exec_processes
            .iter()
            .map(|process| process.command_display.clone())
            .collect();
        self.bottom_pane.set_unified_exec_processes(processes);
    }

    pub(super) fn track_unified_exec_output_chunk(&mut self, call_id: &str, chunk: &[u8]) {
        let Some(process) = self
            .unified_exec_processes
            .iter_mut()
            .find(|process| process.call_id == call_id)
        else {
            return;
        };

        let text = String::from_utf8_lossy(chunk);
        for line in text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
        {
            process.recent_chunks.push(line.to_string());
        }

        const MAX_RECENT_CHUNKS: usize = 3;
        if process.recent_chunks.len() > MAX_RECENT_CHUNKS {
            let drop_count = process.recent_chunks.len() - MAX_RECENT_CHUNKS;
            process.recent_chunks.drain(0..drop_count);
        }
    }

    // ── MCP tool call events ──────────────────────────────────────────────────

    pub(super) fn on_mcp_tool_call_begin(&mut self, ev: McpToolCallBeginEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_mcp_begin(ev), |s| s.handle_mcp_begin_now(ev2));
    }

    pub(super) fn on_mcp_tool_call_end(&mut self, ev: McpToolCallEndEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_mcp_end(ev), |s| s.handle_mcp_end_now(ev2));
    }

    // ── Web search events ─────────────────────────────────────────────────────

    pub(super) fn on_web_search_begin(&mut self, ev: WebSearchBeginEvent) {
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.active_cell = Some(Box::new(history_cell::new_active_web_search_call(
            ev.call_id,
            String::new(),
            self.config.animations,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(super) fn on_web_search_end(&mut self, ev: WebSearchEndEvent) {
        self.flush_answer_stream_with_separator();
        let WebSearchEndEvent {
            call_id,
            query,
            action,
        } = ev;
        let mut handled = false;
        if let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<WebSearchCell>())
            && cell.call_id() == call_id
        {
            cell.update(action.clone(), query.clone());
            cell.complete();
            self.bump_active_cell_revision();
            self.flush_active_cell();
            handled = true;
        }

        if !handled {
            self.add_to_history(history_cell::new_web_search_call(call_id, query, action));
        }
        self.had_work_activity = true;
    }

    // ── Misc small events ─────────────────────────────────────────────────────

    pub(super) fn on_collab_event(&mut self, cell: PlainHistoryCell) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(cell);
        self.request_redraw();
    }

    pub(super) fn on_get_history_entry_response(
        &mut self,
        event: chaos_ipc::protocol::GetHistoryEntryResponseEvent,
    ) {
        let chaos_ipc::protocol::GetHistoryEntryResponseEvent {
            offset,
            log_id,
            entry,
        } = event;
        self.bottom_pane
            .on_history_entry_response(log_id, offset, entry.map(|e| e.text));
    }

    pub(super) fn on_shutdown_complete(&mut self) {
        self.request_immediate_exit();
    }

    pub(super) fn on_turn_diff(&mut self, unified_diff: String) {
        tracing::debug!("TurnDiffEvent: {unified_diff}");
        self.refresh_status_line();
    }

    pub(super) fn on_deprecation_notice(&mut self, event: DeprecationNoticeEvent) {
        let DeprecationNoticeEvent { summary, details } = event;
        self.add_to_history(history_cell::new_deprecation_notice(summary, details));
        self.request_redraw();
    }

    pub(super) fn on_background_event(&mut self, message: String) {
        tracing::debug!("BackgroundEvent: {message}");
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane
            .set_interrupt_hint_visible(/*visible*/ true);
        self.set_status_header(message);
    }

    pub(super) fn on_hook_started(&mut self, event: chaos_ipc::protocol::HookStartedEvent) {
        let label = hook_event_label(event.run.event_name);
        let mut message = format!("Running {label} hook");
        if let Some(status_message) = event.run.status_message
            && !status_message.is_empty()
        {
            message.push_str(": ");
            message.push_str(&status_message);
        }
        self.add_to_history(history_cell::new_info_event(message, /*hint*/ None));
        self.request_redraw();
    }

    pub(super) fn on_hook_completed(&mut self, event: chaos_ipc::protocol::HookCompletedEvent) {
        let status = format!("{:?}", event.run.status).to_lowercase();
        let header = format!("{} hook ({status})", hook_event_label(event.run.event_name));
        let mut lines: Vec<ratatui::text::Line<'static>> = vec![header.into()];
        for entry in event.run.entries {
            let prefix = match entry.kind {
                chaos_ipc::protocol::HookOutputEntryKind::Warning => "warning: ",
                chaos_ipc::protocol::HookOutputEntryKind::Stop => "stop: ",
                chaos_ipc::protocol::HookOutputEntryKind::Feedback => "feedback: ",
                chaos_ipc::protocol::HookOutputEntryKind::Context => "hook context: ",
                chaos_ipc::protocol::HookOutputEntryKind::Error => "error: ",
            };
            lines.push(format!("  {prefix}{}", entry.text).into());
        }
        self.add_to_history(PlainHistoryCell::new(lines));
        self.request_redraw();
    }

    pub(super) fn on_undo_started(&mut self, event: UndoStartedEvent) {
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane
            .set_interrupt_hint_visible(/*visible*/ false);
        let message = event
            .message
            .unwrap_or_else(|| "Undo in progress...".to_string());
        self.set_status_header(message);
    }

    pub(super) fn on_undo_completed(&mut self, event: UndoCompletedEvent) {
        let UndoCompletedEvent { success, message } = event;
        self.bottom_pane.hide_status_indicator();
        let message = message.unwrap_or_else(|| {
            if success {
                "Undo completed successfully.".to_string()
            } else {
                "Undo failed.".to_string()
            }
        });
        if success {
            self.add_info_message(message, /*hint*/ None);
        } else {
            self.add_error_message(message);
        }
    }

    pub(super) fn on_stream_error(&mut self, message: String, additional_details: Option<String>) {
        if self.retry_status_header.is_none() {
            self.retry_status_header = Some(self.current_status.header.clone());
        }
        self.bottom_pane.ensure_status_indicator();
        self.set_status(
            message,
            additional_details,
            StatusDetailsCapitalization::CapitalizeFirst,
            crate::status_indicator_widget::STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
    }

    pub(super) fn on_plan_update(&mut self, update: chaos_ipc::plan_tool::UpdatePlanArgs) {
        self.saw_plan_update_this_turn = true;
        self.add_to_history(history_cell::new_plan_update(update));
    }

    // ── Review mode events ────────────────────────────────────────────────────

    pub(super) fn on_entered_review_mode(&mut self, review: ReviewRequest, from_replay: bool) {
        if self.pre_review_token_info.is_none() {
            self.pre_review_token_info = Some(self.token_info.clone());
        }
        if !from_replay && !self.bottom_pane.is_task_running() {
            self.bottom_pane.set_task_running(/*running*/ true);
        }
        self.is_review_mode = true;
        let hint = review
            .user_facing_hint
            .unwrap_or_else(|| chaos_kern::review_prompts::user_facing_hint(&review.target));
        let banner = format!(">> Code review started: {hint} <<");
        self.add_to_history(history_cell::new_review_status_line(banner));
        self.request_redraw();
    }

    pub(super) fn on_exited_review_mode(&mut self, review: ExitedReviewModeEvent) {
        if let Some(output) = review.review_output {
            self.flush_answer_stream_with_separator();
            self.flush_interrupt_queue();
            self.flush_active_cell();

            if output.findings.is_empty() {
                let explanation = output.overall_explanation.trim().to_string();
                if explanation.is_empty() {
                    tracing::error!("Reviewer failed to output a response.");
                    self.add_to_history(history_cell::new_error_event(
                        "Reviewer failed to output a response.".to_owned(),
                    ));
                } else {
                    let mut rendered: Vec<ratatui::text::Line<'static>> = vec!["".into()];
                    append_markdown(
                        &explanation,
                        /*width*/ None,
                        Some(self.config.cwd.as_path()),
                        &mut rendered,
                    );
                    let body_cell = AgentMessageCell::new(rendered, /*is_first_line*/ false);
                    self.app_event_tx
                        .send(AppEvent::InsertHistoryCell(Box::new(body_cell)));
                }
            }
        }

        self.is_review_mode = false;
        self.restore_pre_review_token_info();
        self.add_to_history(history_cell::new_review_status_line(
            "<< Code review finished >>".to_string(),
        ));
        self.request_redraw();
    }

    // ── User message helpers ──────────────────────────────────────────────────

    pub(super) fn rendered_user_message_event_from_parts(
        message: String,
        text_elements: Vec<TextElement>,
        local_images: Vec<std::path::PathBuf>,
        remote_image_urls: Vec<String>,
    ) -> RenderedUserMessageEvent {
        RenderedUserMessageEvent {
            message,
            remote_image_urls,
            local_images,
            text_elements,
        }
    }

    pub(super) fn rendered_user_message_event_from_event(
        event: &UserMessageEvent,
    ) -> RenderedUserMessageEvent {
        Self::rendered_user_message_event_from_parts(
            event.message.clone(),
            event.text_elements.clone(),
            event.local_images.clone(),
            event.images.clone().unwrap_or_default(),
        )
    }

    pub(super) fn pending_steer_compare_key_from_items(
        items: &[UserInput],
    ) -> PendingSteerCompareKey {
        let mut message = String::new();
        let mut image_count = 0;
        for item in items {
            match item {
                UserInput::Text { text, .. } => message.push_str(text),
                UserInput::Image { .. } | UserInput::LocalImage { .. } => image_count += 1,
                _ => {}
            }
        }
        PendingSteerCompareKey {
            message,
            image_count,
        }
    }

    pub(super) fn pending_steer_compare_key_from_item(
        item: &chaos_ipc::items::UserMessageItem,
    ) -> PendingSteerCompareKey {
        Self::pending_steer_compare_key_from_items(&item.content)
    }

    #[cfg(test)]
    pub(super) fn rendered_user_message_event_from_inputs(
        items: &[UserInput],
    ) -> RenderedUserMessageEvent {
        let mut message = String::new();
        let mut remote_image_urls = Vec::new();
        let mut local_images = Vec::new();
        let mut text_elements = Vec::new();

        for item in items {
            match item {
                UserInput::Text {
                    text,
                    text_elements: current_text_elements,
                } => append_text_with_rebased_elements(
                    &mut message,
                    &mut text_elements,
                    text,
                    current_text_elements.iter().map(|element| {
                        TextElement::new(
                            element.byte_range,
                            element.placeholder(text).map(str::to_string),
                        )
                    }),
                ),
                UserInput::Image { image_url } => remote_image_urls.push(image_url.clone()),
                UserInput::LocalImage { path } => local_images.push(path.clone()),
                _ => {}
            }
        }

        Self::rendered_user_message_event_from_parts(
            message,
            text_elements,
            local_images,
            remote_image_urls,
        )
    }

    pub(super) fn on_user_message_event(&mut self, event: UserMessageEvent) {
        self.last_rendered_user_message_event =
            Some(Self::rendered_user_message_event_from_event(&event));
        let remote_image_urls = event.images.unwrap_or_default();
        if !event.message.trim().is_empty()
            || !event.text_elements.is_empty()
            || !remote_image_urls.is_empty()
        {
            self.add_to_history(history_cell::new_user_prompt(
                event.message,
                event.text_elements,
                event.local_images,
                remote_image_urls,
            ));
        }

        self.needs_final_message_separator = false;
    }

    // ── Replay ────────────────────────────────────────────────────────────────

    pub(super) fn replay_initial_messages(&mut self, events: Vec<chaos_ipc::protocol::EventMsg>) {
        for msg in events {
            self.dispatch_event_msg(
                /*id*/ None,
                msg,
                Some(ReplayKind::ResumeInitialMessages),
            );
        }
    }
}

// ── Exec / MCP / patch immediate-mode handlers ───────────────────────────────
//
// These methods are the "now" variants called by InterruptManager::drain_queue
// once it is safe to process a queued write. They live in this file alongside
// the broader protocol-event dispatch.

impl ChatWidget {
    pub fn handle_exec_end_now(&mut self, ev: ExecCommandEndEvent) {
        use chaos_ipc::protocol::ExecCommandSource;

        enum ExecEndTarget {
            ActiveTracked,
            OrphanHistoryWhileActiveExec,
            NewCell,
        }

        let running = self.running_commands.remove(&ev.call_id);
        if self.suppressed_exec_calls.remove(&ev.call_id) {
            return;
        }
        let (command, parsed, source) = match running {
            Some(rc) => (rc.command, rc.parsed_cmd, rc.source),
            None => (ev.command.clone(), ev.parsed_cmd.clone(), ev.source),
        };
        let is_unified_exec_interaction =
            matches!(source, ExecCommandSource::UnifiedExecInteraction);
        let end_target = match self.active_cell.as_ref() {
            Some(cell) => match cell.as_any().downcast_ref::<ExecCell>() {
                Some(exec_cell)
                    if exec_cell
                        .iter_calls()
                        .any(|call| call.call_id == ev.call_id) =>
                {
                    ExecEndTarget::ActiveTracked
                }
                Some(exec_cell) if exec_cell.is_active() => {
                    ExecEndTarget::OrphanHistoryWhileActiveExec
                }
                Some(_) | None => ExecEndTarget::NewCell,
            },
            None => ExecEndTarget::NewCell,
        };

        let output = if is_unified_exec_interaction {
            CommandOutput {
                exit_code: ev.exit_code,
                formatted_output: String::new(),
                aggregated_output: String::new(),
            }
        } else {
            CommandOutput {
                exit_code: ev.exit_code,
                formatted_output: ev.formatted_output.clone(),
                aggregated_output: ev.aggregated_output.clone(),
            }
        };

        match end_target {
            ExecEndTarget::ActiveTracked => {
                if let Some(cell) = self
                    .active_cell
                    .as_mut()
                    .and_then(|c| c.as_any_mut().downcast_mut::<ExecCell>())
                {
                    let completed = cell.complete_call(&ev.call_id, output, ev.duration);
                    debug_assert!(completed, "active exec cell should contain {}", ev.call_id);
                    if cell.should_flush() {
                        self.flush_active_cell();
                    } else {
                        self.bump_active_cell_revision();
                        self.request_redraw();
                    }
                }
            }
            ExecEndTarget::OrphanHistoryWhileActiveExec => {
                let mut orphan = new_active_exec_command(
                    ev.call_id.clone(),
                    command,
                    parsed,
                    source,
                    ev.interaction_input.clone(),
                    self.config.animations,
                );
                let completed = orphan.complete_call(&ev.call_id, output, ev.duration);
                debug_assert!(
                    completed,
                    "new orphan exec cell should contain {}",
                    ev.call_id
                );
                self.needs_final_message_separator = true;
                self.app_event_tx
                    .send(AppEvent::InsertHistoryCell(Box::new(orphan)));
                self.request_redraw();
            }
            ExecEndTarget::NewCell => {
                self.flush_active_cell();
                let mut cell = new_active_exec_command(
                    ev.call_id.clone(),
                    command,
                    parsed,
                    source,
                    ev.interaction_input.clone(),
                    self.config.animations,
                );
                let completed = cell.complete_call(&ev.call_id, output, ev.duration);
                debug_assert!(completed, "new exec cell should contain {}", ev.call_id);
                if cell.should_flush() {
                    self.add_to_history(cell);
                } else {
                    self.active_cell = Some(Box::new(cell));
                    self.bump_active_cell_revision();
                    self.request_redraw();
                }
            }
        }
        self.had_work_activity = true;
    }

    pub fn handle_patch_apply_end_now(&mut self, event: chaos_ipc::protocol::PatchApplyEndEvent) {
        if !event.success {
            self.add_to_history(history_cell::new_patch_apply_failure(event.stderr));
        }
        self.had_work_activity = true;
    }

    pub fn handle_exec_approval_now(&mut self, ev: ExecApprovalRequestEvent) {
        use super::core::Notification;
        self.flush_answer_stream_with_separator();
        let command = shlex::try_join(ev.command.iter().map(String::as_str))
            .unwrap_or_else(|_| ev.command.join(" "));
        self.notify(Notification::ExecApprovalRequested { command });
        let available_decisions = ev.effective_available_decisions();
        let request = ApprovalRequest::Exec {
            process_id: self.process_id.unwrap_or_default(),
            process_label: None,
            id: ev.effective_approval_id(),
            command: ev.command,
            reason: ev.reason,
            available_decisions,
            network_approval_context: ev.network_approval_context,
            additional_permissions: ev.additional_permissions,
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub fn handle_apply_patch_approval_now(&mut self, ev: ApplyPatchApprovalRequestEvent) {
        use super::core::Notification;
        self.flush_answer_stream_with_separator();
        let request = ApprovalRequest::ApplyPatch {
            process_id: self.process_id.unwrap_or_default(),
            process_label: None,
            id: ev.call_id,
            reason: ev.reason,
            changes: ev.changes.clone(),
            cwd: self.config.cwd.clone(),
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
        self.notify(Notification::EditApprovalRequested {
            cwd: self.config.cwd.clone(),
            changes: ev.changes.keys().cloned().collect(),
        });
    }

    pub fn handle_elicitation_request_now(
        &mut self,
        ev: chaos_ipc::approvals::ElicitationRequestEvent,
    ) {
        use super::core::Notification;
        use chaos_ipc::approvals::ElicitationRequest;
        self.flush_answer_stream_with_separator();
        self.notify(Notification::ElicitationRequested {
            server_name: ev.server_name.clone(),
        });
        let process_id = self.process_id.unwrap_or_default();
        if let Some(request) = McpServerElicitationFormRequest::from_event(process_id, ev.clone()) {
            self.bottom_pane
                .push_mcp_server_elicitation_request(request);
        } else {
            let url = match &ev.request {
                ElicitationRequest::Url { url, .. } => Some(url.clone()),
                ElicitationRequest::Form { .. } => None,
            };
            let request = ApprovalRequest::McpElicitation {
                process_id,
                process_label: None,
                server_name: ev.server_name,
                request_id: ev.id,
                message: ev.request.message().to_string(),
                url,
            };
            self.bottom_pane
                .push_approval_request(request, &self.config.features);
        }
        self.request_redraw();
    }

    pub fn push_approval_request(&mut self, request: ApprovalRequest) {
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub fn push_mcp_server_elicitation_request(
        &mut self,
        request: McpServerElicitationFormRequest,
    ) {
        self.bottom_pane
            .push_mcp_server_elicitation_request(request);
        self.request_redraw();
    }

    pub fn handle_request_user_input_now(&mut self, ev: RequestUserInputEvent) {
        use super::core::Notification;
        self.flush_answer_stream_with_separator();
        self.notify(Notification::UserInputRequested {
            question_count: ev.questions.len(),
            summary: Notification::user_input_request_summary(&ev.questions),
        });
        self.bottom_pane.push_user_input_request(ev);
        self.request_redraw();
    }

    pub fn handle_request_permissions_now(&mut self, ev: RequestPermissionsEvent) {
        self.flush_answer_stream_with_separator();
        let request = ApprovalRequest::Permissions {
            process_id: self.process_id.unwrap_or_default(),
            process_label: None,
            call_id: ev.call_id,
            reason: ev.reason,
            permissions: ev.permissions,
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub fn handle_exec_begin_now(&mut self, ev: ExecCommandBeginEvent) {
        use chaos_ipc::protocol::ExecCommandSource;
        self.bottom_pane.ensure_status_indicator();
        self.running_commands.insert(
            ev.call_id.clone(),
            RunningCommand {
                command: ev.command.clone(),
                parsed_cmd: ev.parsed_cmd.clone(),
                source: ev.source,
            },
        );
        let is_wait_interaction = matches!(ev.source, ExecCommandSource::UnifiedExecInteraction)
            && ev
                .interaction_input
                .as_deref()
                .map(str::is_empty)
                .unwrap_or(true);
        let command_display = ev.command.join(" ");
        let should_suppress_unified_wait = is_wait_interaction
            && self
                .last_unified_wait
                .as_ref()
                .is_some_and(|wait| wait.is_duplicate(&command_display));
        if is_wait_interaction {
            self.last_unified_wait = Some(UnifiedExecWaitState::new(command_display));
        } else {
            self.last_unified_wait = None;
        }
        if should_suppress_unified_wait {
            self.suppressed_exec_calls.insert(ev.call_id);
            return;
        }
        let interaction_input = ev.interaction_input.clone();
        if let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|c| c.as_any_mut().downcast_mut::<ExecCell>())
            && let Some(new_exec) = cell.with_added_call(
                ev.call_id.clone(),
                ev.command.clone(),
                ev.parsed_cmd.clone(),
                ev.source,
                interaction_input.clone(),
            )
        {
            *cell = new_exec;
            self.bump_active_cell_revision();
        } else {
            self.flush_active_cell();
            self.active_cell = Some(Box::new(new_active_exec_command(
                ev.call_id.clone(),
                ev.command.clone(),
                ev.parsed_cmd,
                ev.source,
                interaction_input,
                self.config.animations,
            )));
            self.bump_active_cell_revision();
        }
        self.request_redraw();
    }

    pub fn handle_mcp_begin_now(&mut self, ev: McpToolCallBeginEvent) {
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.active_cell = Some(Box::new(history_cell::new_active_mcp_tool_call(
            ev.call_id,
            ev.invocation,
            self.config.animations,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub fn handle_mcp_end_now(&mut self, ev: McpToolCallEndEvent) {
        self.flush_answer_stream_with_separator();
        let McpToolCallEndEvent {
            call_id,
            invocation,
            duration,
            result,
        } = ev;
        let extra_cell = match self
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<McpToolCallCell>())
        {
            Some(cell) if cell.call_id() == call_id => cell.complete(duration, result),
            _ => {
                self.flush_active_cell();
                let mut cell = history_cell::new_active_mcp_tool_call(
                    call_id,
                    invocation,
                    self.config.animations,
                );
                let extra_cell = cell.complete(duration, result);
                self.active_cell = Some(Box::new(cell));
                extra_cell
            }
        };
        self.flush_active_cell();
        if let Some(extra) = extra_cell {
            self.add_boxed_history(extra);
        }
        self.had_work_activity = true;
    }
}

// ---------------------------------------------------------------------------
// Keyboard / UI event handlers
// ---------------------------------------------------------------------------

impl ChatWidget {
    pub fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'c') => {
                self.on_ctrl_c();
                return;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'d') => {
                if self.on_ctrl_d() {
                    return;
                }
                self.bottom_pane.clear_quit_shortcut_hint();
                self.quit_shortcut_expires_at = None;
                self.quit_shortcut_key = None;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && c.eq_ignore_ascii_case(&'v') =>
            {
                match paste_image_to_temp_png() {
                    Ok((path, info)) => {
                        tracing::debug!(
                            "pasted image size={}x{} format={}",
                            info.width,
                            info.height,
                            info.encoded_format.label()
                        );
                        self.attach_image(path);
                    }
                    Err(err) => {
                        tracing::warn!("failed to paste image: {err}");
                        self.add_to_history(history_cell::new_error_event(format!(
                            "Failed to paste image: {err}",
                        )));
                    }
                }
                return;
            }
            other if other.kind == KeyEventKind::Press => {
                self.bottom_pane.clear_quit_shortcut_hint();
                self.quit_shortcut_expires_at = None;
                self.quit_shortcut_key = None;
            }
            _ => {}
        }

        if key_event.kind == KeyEventKind::Press
            && self.queued_message_edit_binding.is_press(key_event)
            && !self.queued_user_messages.is_empty()
        {
            if let Some(user_message) = self.queued_user_messages.pop_back() {
                self.restore_user_message_to_composer(user_message);
                self.refresh_pending_input_preview();
                self.request_redraw();
            }
            return;
        }

        if matches!(key_event.code, KeyCode::Esc)
            && matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && !self.pending_steers.is_empty()
            && self.bottom_pane.is_task_running()
            && self.bottom_pane.no_modal_or_popup_active()
        {
            self.submit_pending_steers_after_interrupt = true;
            if !self.submit_op(Op::Interrupt) {
                self.submit_pending_steers_after_interrupt = false;
            }
            return;
        }

        match key_event {
            KeyEvent {
                code: KeyCode::BackTab,
                kind: KeyEventKind::Press,
                ..
            } if self.collaboration_modes_enabled()
                && !self.bottom_pane.is_task_running()
                && self.bottom_pane.no_modal_or_popup_active() =>
            {
                self.cycle_collaboration_mode();
            }
            _ => match self.bottom_pane.handle_key_event(key_event) {
                InputResult::Submitted {
                    text,
                    text_elements,
                } => {
                    let local_images = self
                        .bottom_pane
                        .take_recent_submission_images_with_placeholders();
                    let remote_image_urls = self.take_remote_image_urls();
                    let user_message = UserMessage {
                        text,
                        local_images,
                        remote_image_urls,
                        text_elements,
                        mention_bindings: self
                            .bottom_pane
                            .take_recent_submission_mention_bindings(),
                    };
                    if user_message.text.is_empty()
                        && user_message.local_images.is_empty()
                        && user_message.remote_image_urls.is_empty()
                    {
                        return;
                    }
                    let should_submit_now =
                        self.is_session_configured() && !self.is_plan_streaming_in_tui();
                    if should_submit_now {
                        // Submitted is emitted when user submits.
                        // Reset any reasoning header only when we are actually submitting a turn.
                        self.reasoning_buffer.clear();
                        self.full_reasoning_buffer.clear();
                        self.set_status_header(String::from("Working"));
                        self.submit_user_message(user_message);
                    } else {
                        self.queue_user_message(user_message);
                    }
                }
                InputResult::Queued {
                    text,
                    text_elements,
                } => {
                    let local_images = self
                        .bottom_pane
                        .take_recent_submission_images_with_placeholders();
                    let remote_image_urls = self.take_remote_image_urls();
                    let user_message = UserMessage {
                        text,
                        local_images,
                        remote_image_urls,
                        text_elements,
                        mention_bindings: self
                            .bottom_pane
                            .take_recent_submission_mention_bindings(),
                    };
                    self.queue_user_message(user_message);
                }
                InputResult::Command(cmd) => {
                    self.dispatch_command(cmd);
                }
                InputResult::CommandWithArgs(cmd, args, text_elements) => {
                    self.dispatch_command_with_args(cmd, args, text_elements);
                }
                InputResult::None => {}
            },
        }
    }

    /// Attach a local image to the composer when the active model supports image inputs.
    ///
    /// When the model does not advertise image support, we keep the draft unchanged and surface a
    /// warning event so users can switch models or remove attachments.
    pub fn attach_image(&mut self, path: PathBuf) {
        if !self.current_model_supports_images() {
            self.add_to_history(history_cell::new_warning_event(
                self.image_inputs_not_supported_message(),
            ));
            self.request_redraw();
            return;
        }
        tracing::info!("attach_image path={path:?}");
        self.bottom_pane.attach_image(path);
        self.request_redraw();
    }

    pub fn composer_text_with_pending(&self) -> String {
        self.bottom_pane.composer_text_with_pending()
    }

    pub fn apply_external_edit(&mut self, text: String) {
        self.bottom_pane.apply_external_edit(text);
        self.request_redraw();
    }

    pub fn external_editor_state(&self) -> ExternalEditorState {
        self.external_editor_state
    }

    pub fn set_external_editor_state(&mut self, state: ExternalEditorState) {
        self.external_editor_state = state;
    }

    pub fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.bottom_pane.set_footer_hint_override(items);
    }

    pub fn show_selection_view(&mut self, params: SelectionViewParams) {
        self.bottom_pane.show_selection_view(params);
        self.request_redraw();
    }

    pub fn no_modal_or_popup_active(&self) -> bool {
        self.bottom_pane.no_modal_or_popup_active()
    }

    pub fn can_launch_external_editor(&self) -> bool {
        self.bottom_pane.can_launch_external_editor()
    }

    pub fn can_run_ctrl_l_clear_now(&mut self) -> bool {
        // Ctrl+L is not a slash command, but it follows /clear's rule:
        // block while a task is running.
        if !self.bottom_pane.is_task_running() {
            return true;
        }

        let message = "Ctrl+L is disabled while a task is in progress.".to_string();
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();
        false
    }

    pub fn handle_paste(&mut self, text: String) {
        self.bottom_pane.handle_paste(text);
    }

    // Returns true if caller should skip rendering this frame (a future frame is scheduled).
    pub fn handle_paste_burst_tick(&mut self, frame_requester: FrameRequester) -> bool {
        if self.bottom_pane.flush_paste_burst_if_due() {
            // A paste just flushed; request an immediate redraw and skip this frame.
            self.request_redraw();
            true
        } else if self.bottom_pane.is_in_paste_burst() {
            // While capturing a burst, schedule a follow-up tick and skip this frame
            // to avoid redundant renders between ticks.
            frame_requester.schedule_frame_in(
                crate::bottom_pane::ChatComposer::recommended_paste_flush_delay(),
            );
            true
        } else {
            false
        }
    }

    /// Handles a Ctrl+C press at the chat-widget layer.
    ///
    /// The first press arms a time-bounded quit shortcut and shows a footer hint via the bottom
    /// pane. If cancellable work is active, Ctrl+C also submits `Op::Interrupt` after the shortcut
    /// is armed; this interrupts the turn but intentionally preserves background terminals.
    ///
    /// If the same quit shortcut is pressed again before expiry, this requests a shutdown-first
    /// quit.
    fn on_ctrl_c(&mut self) {
        let key = key_hint::ctrl(KeyCode::Char('c'));
        let modal_or_popup_active = !self.bottom_pane.no_modal_or_popup_active();
        if self.bottom_pane.on_ctrl_c() == CancellationEvent::Handled {
            if DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
                if modal_or_popup_active {
                    self.quit_shortcut_expires_at = None;
                    self.quit_shortcut_key = None;
                    self.bottom_pane.clear_quit_shortcut_hint();
                } else {
                    self.arm_quit_shortcut(key);
                }
            }
            return;
        }

        if !DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
            if self.is_cancellable_work_active() {
                self.submit_op(Op::Interrupt);
            } else {
                self.request_quit_without_confirmation();
            }
            return;
        }

        if self.quit_shortcut_active_for(key) {
            self.quit_shortcut_expires_at = None;
            self.quit_shortcut_key = None;
            self.request_quit_without_confirmation();
            return;
        }

        self.arm_quit_shortcut(key);

        if self.is_cancellable_work_active() {
            self.submit_op(Op::Interrupt);
        }
    }

    /// Handles a Ctrl+D press at the chat-widget layer.
    ///
    /// Ctrl-D only participates in quit when the composer is empty and no modal/popup is active.
    /// Otherwise it should be routed to the active view and not attempt to quit.
    fn on_ctrl_d(&mut self) -> bool {
        let key = key_hint::ctrl(KeyCode::Char('d'));
        if !DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
            if !self.bottom_pane.composer_is_empty() || !self.bottom_pane.no_modal_or_popup_active()
            {
                return false;
            }

            self.request_quit_without_confirmation();
            return true;
        }

        if self.quit_shortcut_active_for(key) {
            self.quit_shortcut_expires_at = None;
            self.quit_shortcut_key = None;
            self.request_quit_without_confirmation();
            return true;
        }

        if !self.bottom_pane.composer_is_empty() || !self.bottom_pane.no_modal_or_popup_active() {
            return false;
        }

        self.arm_quit_shortcut(key);
        true
    }

    /// True if `key` matches the armed quit shortcut and the window has not expired.
    fn quit_shortcut_active_for(&self, key: KeyBinding) -> bool {
        self.quit_shortcut_key == Some(key)
            && self
                .quit_shortcut_expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
    }

    /// Arm the double-press quit shortcut and show the footer hint.
    ///
    /// This keeps the state machine (`quit_shortcut_*`) in `ChatWidget`, since
    /// it is the component that interprets Ctrl+C vs Ctrl+D and decides whether
    /// quitting is currently allowed, while delegating rendering to `BottomPane`.
    fn arm_quit_shortcut(&mut self, key: KeyBinding) {
        self.quit_shortcut_expires_at = Instant::now()
            .checked_add(QUIT_SHORTCUT_TIMEOUT)
            .or_else(|| Some(Instant::now()));
        self.quit_shortcut_key = Some(key);
        self.bottom_pane.show_quit_shortcut_hint(key);
    }

    // Review mode counts as cancellable work so Ctrl+C interrupts instead of quitting.
    fn is_cancellable_work_active(&self) -> bool {
        self.bottom_pane.is_task_running() || self.is_review_mode
    }

    fn is_plan_streaming_in_tui(&self) -> bool {
        self.plan_stream_controller.is_some()
    }

    pub fn composer_is_empty(&self) -> bool {
        self.bottom_pane.composer_is_empty()
    }

    pub fn submit_user_message_with_mode(
        &mut self,
        text: String,
        mut collaboration_mode: CollaborationModeMask,
    ) {
        if collaboration_mode.mode == Some(ModeKind::Plan)
            && let Some(effort) = self.config.plan_mode_reasoning_effort
        {
            collaboration_mode.reasoning_effort = Some(Some(effort));
        }
        if self.agent_turn_running
            && self.active_collaboration_mask.as_ref() != Some(&collaboration_mode)
        {
            self.add_error_message(
                "Cannot switch collaboration mode while a turn is running.".to_string(),
            );
            return;
        }
        self.set_collaboration_mask(collaboration_mode);
        let should_queue = self.is_plan_streaming_in_tui();
        let user_message = UserMessage {
            text,
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        };
        if should_queue {
            self.queue_user_message(user_message);
        } else {
            self.submit_user_message(user_message);
        }
    }

    /// True when the UI is in the regular composer state with no running task,
    /// no modal overlay (e.g. approvals or status indicator), and no composer popups.
    /// In this state Esc-Esc backtracking is enabled.
    pub fn is_normal_backtrack_mode(&self) -> bool {
        self.bottom_pane.is_normal_backtrack_mode()
    }

    pub fn insert_str(&mut self, text: &str) {
        self.bottom_pane.insert_str(text);
    }

    /// Replace the composer content with the provided text and reset cursor.
    pub fn set_composer_text(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        self.bottom_pane
            .set_composer_text(text, text_elements, local_image_paths);
    }

    pub fn set_remote_image_urls(&mut self, remote_image_urls: Vec<String>) {
        self.bottom_pane.set_remote_image_urls(remote_image_urls);
    }

    pub(super) fn take_remote_image_urls(&mut self) -> Vec<String> {
        self.bottom_pane.take_remote_image_urls()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn remote_image_urls(&self) -> Vec<String> {
        self.bottom_pane.remote_image_urls()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn queued_user_message_texts(&self) -> Vec<String> {
        self.queued_user_messages
            .iter()
            .map(|message| message.text.clone())
            .collect()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn pending_process_approvals(&self) -> &[String] {
        self.bottom_pane.pending_process_approvals()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn has_active_view(&self) -> bool {
        self.bottom_pane.has_active_view()
    }

    pub fn show_esc_backtrack_hint(&mut self) {
        self.bottom_pane.show_esc_backtrack_hint();
    }

    pub fn clear_esc_backtrack_hint(&mut self) {
        self.bottom_pane.clear_esc_backtrack_hint();
    }

    /// Forward an `Op` directly to chaos.
    pub fn submit_op(&mut self, op: Op) -> bool {
        // Record outbound operation for session replay fidelity.
        crate::session_log::log_outbound_op(&op);
        if matches!(&op, Op::Review { .. }) && !self.bottom_pane.is_task_running() {
            self.bottom_pane.set_task_running(/*running*/ true);
        }
        if let Err(e) = self.chaos_op_tx.send(op) {
            tracing::error!("failed to submit op: {e}");
            return false;
        }
        true
    }
}
