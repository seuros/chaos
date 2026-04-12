//! Streaming message handling for `ChatWidget`.
//!
//! This module groups the methods that drive incremental content delivery:
//! stream delta ingestion, commit-tick pacing, plan and reasoning delta
//! processing, and the interrupt-queue gating that keeps exec and patch events
//! in order with respect to active write cycles.

use std::time::Instant;

use chaos_ipc::items::AgentMessageContent;
use chaos_ipc::items::AgentMessageItem;
use chaos_ipc::models::MessagePhase;
use chaos_syslog::RuntimeMetricsSummary;
use ratatui::style::Stylize;

use crate::app_event::AppEvent;
use crate::history_cell;
use crate::streaming::commit_tick::CommitTickScope;
use crate::streaming::commit_tick::run_commit_tick;

use super::ChatWidget;
use super::core::extract_first_bold;
use super::core::has_websocket_timing_metrics;
use super::interrupts::InterruptManager;

impl ChatWidget {
    /// Flush any accumulated unified-exec wait streak into a history cell.
    pub(super) fn flush_unified_exec_wait_streak(&mut self) {
        let Some(wait) = self.unified_exec_wait_streak.take() else {
            return;
        };
        self.needs_final_message_separator = true;
        let cell = history_cell::new_unified_exec_interaction(wait.command_display, String::new());
        self.app_event_tx
            .send(AppEvent::InsertHistoryCell(Box::new(cell)));
        self.restore_reasoning_status_header();
    }

    /// Finalize any active streaming answer and flush it into history.
    pub(super) fn flush_answer_stream_with_separator(&mut self) {
        if let Some(mut controller) = self.stream_controller.take()
            && let Some(cell) = controller.finalize()
        {
            self.add_boxed_history(cell);
        }
        self.adaptive_chunking.reset();
    }

    /// Returns `true` when all stream controllers have no queued lines.
    pub(super) fn stream_controllers_idle(&self) -> bool {
        self.stream_controller
            .as_ref()
            .map(|controller| controller.queued_lines() == 0)
            .unwrap_or(true)
            && self
                .plan_stream_controller
                .as_ref()
                .map(|controller| controller.queued_lines() == 0)
                .unwrap_or(true)
    }

    /// Re-show the status indicator after commentary completion once all queues drain.
    pub(super) fn maybe_restore_status_indicator_after_stream_idle(&mut self) {
        if !self.pending_status_indicator_restore
            || !self.bottom_pane.is_task_running()
            || !self.stream_controllers_idle()
        {
            return;
        }

        self.bottom_pane.ensure_status_indicator();
        self.set_status(
            self.current_status.header.clone(),
            self.current_status.details.clone(),
            crate::status_indicator_widget::StatusDetailsCapitalization::Preserve,
            self.current_status.details_max_lines,
        );
        self.pending_status_indicator_restore = false;
    }

    /// Restore the status header derived from the most recent reasoning buffer.
    pub(super) fn restore_reasoning_status_header(&mut self) {
        if let Some(header) = extract_first_bold(&self.reasoning_buffer) {
            self.set_status_header(header);
        } else if self.bottom_pane.is_task_running() {
            self.set_status_header(String::from("Working"));
        }
    }

    // ── Agent message / delta handlers ───────────────────────────────────────

    pub(super) fn finalize_completed_assistant_message(&mut self, message: Option<&str>) {
        if self.stream_controller.is_none()
            && let Some(message) = message
            && !message.is_empty()
        {
            self.handle_streaming_delta(message.to_string());
        }
        self.flush_answer_stream_with_separator();
        self.handle_stream_finished();
        self.request_redraw();
    }

    pub(super) fn on_agent_message(&mut self, message: String) {
        self.finalize_completed_assistant_message(Some(&message));
    }

    pub(super) fn on_agent_message_delta(&mut self, delta: String) {
        self.handle_streaming_delta(delta);
    }

    pub(super) fn on_plan_delta(&mut self, delta: String) {
        use chaos_ipc::config_types::ModeKind;
        if self.active_mode_kind() != ModeKind::Plan {
            return;
        }
        if !self.plan_item_active {
            self.plan_item_active = true;
            self.plan_delta_buffer.clear();
        }
        self.plan_delta_buffer.push_str(&delta);
        self.flush_unified_exec_wait_streak();
        self.flush_active_cell();

        if self.plan_stream_controller.is_none() {
            self.plan_stream_controller =
                Some(crate::streaming::controller::PlanStreamController::new(
                    self.last_rendered_width.get().map(|w| w.saturating_sub(4)),
                    &self.config.cwd,
                ));
        }
        if let Some(controller) = self.plan_stream_controller.as_mut()
            && controller.push(&delta)
        {
            self.app_event_tx.send(AppEvent::StartCommitAnimation);
            self.run_catch_up_commit_tick();
        }
        self.request_redraw();
    }

    pub(super) fn on_plan_item_completed(&mut self, text: String) {
        let streamed_plan = self.plan_delta_buffer.trim().to_string();
        let plan_text = if text.trim().is_empty() {
            streamed_plan
        } else {
            text
        };
        if !plan_text.trim().is_empty() {
            self.last_copyable_output = Some(plan_text.clone());
        }
        let should_restore_after_stream = self.plan_stream_controller.is_some();
        self.plan_delta_buffer.clear();
        self.plan_item_active = false;
        self.saw_plan_item_this_turn = true;
        let finalized_streamed_cell =
            if let Some(mut controller) = self.plan_stream_controller.take() {
                controller.finalize()
            } else {
                None
            };
        if let Some(cell) = finalized_streamed_cell {
            self.add_boxed_history(cell);
        } else if !plan_text.is_empty() {
            self.add_to_history(history_cell::new_proposed_plan(plan_text, &self.config.cwd));
        }
        if should_restore_after_stream {
            self.pending_status_indicator_restore = true;
            self.maybe_restore_status_indicator_after_stream_idle();
        }
    }

    pub(super) fn on_agent_reasoning_delta(&mut self, delta: String) {
        self.reasoning_buffer.push_str(&delta);

        if self.unified_exec_wait_streak.is_some() {
            self.request_redraw();
            return;
        }

        if let Some(header) = extract_first_bold(&self.reasoning_buffer) {
            self.set_status_header(header);
        }
        self.request_redraw();
    }

    pub(super) fn on_agent_reasoning_final(&mut self) {
        self.full_reasoning_buffer.push_str(&self.reasoning_buffer);
        if !self.full_reasoning_buffer.is_empty() {
            let cell = history_cell::new_reasoning_summary_block(
                self.full_reasoning_buffer.clone(),
                &self.config.cwd,
            );
            self.add_boxed_history(cell);
        }
        self.reasoning_buffer.clear();
        self.full_reasoning_buffer.clear();
        self.request_redraw();
    }

    pub(super) fn on_reasoning_section_break(&mut self) {
        self.full_reasoning_buffer.push_str(&self.reasoning_buffer);
        self.full_reasoning_buffer.push_str("\n\n");
        self.reasoning_buffer.clear();
    }

    /// Handle completion of an `AgentMessage` turn item.
    ///
    /// Commentary completion sets a deferred restore flag so the status row
    /// returns once stream queues are idle.
    pub(super) fn on_agent_message_item_completed(&mut self, item: AgentMessageItem) {
        let mut message = String::new();
        for content in &item.content {
            match content {
                AgentMessageContent::Text { text } => message.push_str(text),
            }
        }
        self.finalize_completed_assistant_message(
            (!message.is_empty()).then_some(message.as_str()),
        );
        self.pending_status_indicator_restore = match item.phase {
            Some(MessagePhase::FinalAnswer) | None => false,
            Some(MessagePhase::Commentary) => true,
        };
        self.maybe_restore_status_indicator_after_stream_idle();
    }

    // ── Commit-tick pacing ────────────────────────────────────────────────────

    /// Periodic tick for stream commits.
    pub fn on_commit_tick(&mut self) {
        self.run_commit_tick();
    }

    pub fn pre_draw_tick(&mut self) {
        self.bottom_pane.pre_draw_tick();
    }

    fn run_commit_tick(&mut self) {
        self.run_commit_tick_with_scope(CommitTickScope::AnyMode);
    }

    pub(super) fn run_catch_up_commit_tick(&mut self) {
        self.run_commit_tick_with_scope(CommitTickScope::CatchUpOnly);
    }

    fn run_commit_tick_with_scope(&mut self, scope: CommitTickScope) {
        let now = Instant::now();
        let outcome = run_commit_tick(
            &mut self.adaptive_chunking,
            self.stream_controller.as_mut(),
            self.plan_stream_controller.as_mut(),
            scope,
            now,
        );
        for cell in outcome.cells {
            self.bottom_pane.hide_status_indicator();
            self.add_boxed_history(cell);
        }

        if outcome.has_controller && outcome.all_idle {
            self.maybe_restore_status_indicator_after_stream_idle();
            self.app_event_tx.send(AppEvent::StopCommitAnimation);
        }

        if self.agent_turn_running {
            self.refresh_runtime_metrics();
        }
    }

    // ── Interrupt queue ───────────────────────────────────────────────────────

    pub(super) fn flush_interrupt_queue(&mut self) {
        let mut mgr = std::mem::take(&mut self.interrupts);
        mgr.flush_all(self);
        self.interrupts = mgr;
    }

    #[inline]
    pub(super) fn defer_or_handle(
        &mut self,
        push: impl FnOnce(&mut InterruptManager),
        handle: impl FnOnce(&mut Self),
    ) {
        if self.stream_controller.is_some() || !self.interrupts.is_empty() {
            push(&mut self.interrupts);
        } else {
            handle(self);
        }
    }

    pub(super) fn handle_stream_finished(&mut self) {
        if self.task_complete_pending {
            self.bottom_pane.hide_status_indicator();
            self.task_complete_pending = false;
        }
        self.flush_interrupt_queue();
    }

    #[inline]
    pub(super) fn handle_streaming_delta(&mut self, delta: String) {
        self.flush_unified_exec_wait_streak();
        self.flush_active_cell();

        if self.stream_controller.is_none() {
            if self.needs_final_message_separator && self.had_work_activity {
                let elapsed_seconds = self
                    .bottom_pane
                    .status_widget()
                    .map(crate::status_indicator_widget::StatusIndicatorWidget::elapsed_seconds)
                    .map(|current| self.worked_elapsed_from(current));
                self.add_to_history(history_cell::FinalMessageSeparator::new(
                    elapsed_seconds,
                    /*runtime_metrics*/ None,
                ));
                self.needs_final_message_separator = false;
                self.had_work_activity = false;
            } else if self.needs_final_message_separator {
                self.needs_final_message_separator = false;
            }
            self.stream_controller = Some(crate::streaming::controller::StreamController::new(
                self.last_rendered_width.get().map(|w| w.saturating_sub(2)),
                &self.config.cwd,
            ));
        }
        if let Some(controller) = self.stream_controller.as_mut()
            && controller.push(&delta)
        {
            self.app_event_tx.send(AppEvent::StartCommitAnimation);
            self.run_catch_up_commit_tick();
        }
        self.request_redraw();
    }

    pub(super) fn worked_elapsed_from(&mut self, current_elapsed: u64) -> u64 {
        let baseline = match self.last_separator_elapsed_secs {
            Some(last) if current_elapsed < last => 0,
            Some(last) => last,
            None => 0,
        };
        let elapsed = current_elapsed.saturating_sub(baseline);
        self.last_separator_elapsed_secs = Some(current_elapsed);
        elapsed
    }

    // ── Runtime metrics ───────────────────────────────────────────────────────

    pub(super) fn collect_runtime_metrics_delta(&mut self) {
        if let Some(delta) = self.session_telemetry.runtime_metrics_summary() {
            self.apply_runtime_metrics_delta(delta);
        }
    }

    pub(super) fn apply_runtime_metrics_delta(&mut self, delta: RuntimeMetricsSummary) {
        let should_log_timing = has_websocket_timing_metrics(delta);
        self.turn_runtime_metrics.merge(delta);
        if should_log_timing {
            self.log_websocket_timing_totals(delta);
        }
    }

    pub(super) fn log_websocket_timing_totals(&mut self, delta: RuntimeMetricsSummary) {
        if let Some(label) = history_cell::runtime_metrics_label(delta.responses_api_summary()) {
            self.add_plain_history_lines(vec![
                vec!["• ".dim(), format!("WebSocket timing: {label}").dark_gray()].into(),
            ]);
        }
    }

    pub(super) fn refresh_runtime_metrics(&mut self) {
        self.collect_runtime_metrics_delta();
    }
}
