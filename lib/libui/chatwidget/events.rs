//! Protocol event dispatch for `ChatWidget`.
//!
//! This module collects the methods that consume `EventMsg` values from the
//! chaos-kern event stream and translate them into widget state mutations.
//! The public surface is `handle_codex_event` and `handle_codex_event_replay`
//! which route through the private `dispatch_event_msg` dispatcher.

use chaos_ipc::approvals::ElicitationRequestEvent;
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

use crate::app_event::AppEvent;
use crate::exec_cell::ExecCell;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::WebSearchCell;
use crate::markdown::append_markdown;
use crate::multi_agents;
use crate::status_indicator_widget::StatusDetailsCapitalization;

use super::ChatWidget;
use super::core::PendingSteerCompareKey;
use super::core::RateLimitErrorKind;
use super::core::RenderedUserMessageEvent;
use super::core::ReplayKind;
use super::core::UnifiedExecProcessSummary;
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
