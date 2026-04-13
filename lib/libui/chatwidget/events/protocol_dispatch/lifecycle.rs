//! Session and task lifecycle event handlers.

use chaos_ipc::protocol::McpStartupCompleteEvent;
use chaos_ipc::protocol::McpStartupStatus;
use chaos_ipc::protocol::McpStartupUpdateEvent;
use chaos_ipc::protocol::TurnAbortReason;
use chaos_kern::config::Constrained;

use crate::history_cell;

use super::super::super::ChatWidget;
use super::super::super::core::Notification;
use super::super::super::core::ReplayKind;
use super::super::super::core::UserMessage;
use super::super::super::core::merge_user_messages;

impl ChatWidget {
    // ── Session / process events ──────────────────────────────────────────────

    pub(crate) fn on_session_configured(
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

    pub(crate) fn on_process_name_updated(
        &mut self,
        event: chaos_ipc::protocol::ProcessNameUpdatedEvent,
    ) {
        if self.process_id == Some(event.process_id) {
            self.process_name = event.process_name;
            self.request_redraw();
        }
    }

    // ── Turn lifecycle events ─────────────────────────────────────────────────

    pub(crate) fn on_task_started(&mut self) {
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

    pub(crate) fn on_task_complete(
        &mut self,
        last_agent_message: Option<String>,
        from_replay: bool,
    ) {
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

    pub(crate) fn finalize_turn(&mut self) {
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

    pub(crate) fn on_server_overloaded_error(&mut self, message: String) {
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

    pub(crate) fn on_error(&mut self, message: String) {
        self.submit_pending_steers_after_interrupt = false;
        self.finalize_turn();
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();
        self.maybe_send_next_queued_input();
    }

    pub(crate) fn on_warning(&mut self, message: impl Into<String>) {
        self.add_to_history(history_cell::new_warning_event(message.into()));
        self.request_redraw();
    }

    // ── MCP startup events ────────────────────────────────────────────────────

    pub(crate) fn on_mcp_startup_update(&mut self, ev: McpStartupUpdateEvent) {
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

    pub(crate) fn on_mcp_startup_complete(&mut self, ev: McpStartupCompleteEvent) {
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

    pub(crate) fn on_interrupted_turn(&mut self, reason: TurnAbortReason) {
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
            let pending_steers: Vec<UserMessage> = self
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

    // ── Replay ────────────────────────────────────────────────────────────────

    pub(crate) fn replay_initial_messages(&mut self, events: Vec<chaos_ipc::protocol::EventMsg>) {
        for msg in events {
            self.dispatch_event_msg(
                /*id*/ None,
                msg,
                Some(ReplayKind::ResumeInitialMessages),
            );
        }
    }
}
