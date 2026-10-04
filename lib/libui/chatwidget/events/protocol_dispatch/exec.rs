//! Command execution event handlers: begin, output delta, end, terminal
//! interaction, patch apply, and exec process tracking.

use chaos_ipc::protocol::ExecCommandBeginEvent;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::ExecCommandOutputDeltaEvent;
use chaos_ipc::protocol::ExecCommandSource;
use chaos_ipc::protocol::PatchApplyBeginEvent;
use chaos_ipc::protocol::TerminalInteractionEvent;

use crate::exec_cell::ExecCell;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell;
use crate::status_indicator_widget::StatusDetailsCapitalization;

use super::super::super::ChatWidget;
use super::super::super::core::ExecProcessSummary;
use super::super::super::core::ExecWaitStreak;
use super::super::super::core::is_exec_source;
use super::super::super::core::is_standard_tool_call;

impl ChatWidget {
    // ── Exec events ───────────────────────────────────────────────────────────

    pub(crate) fn on_exec_command_begin(&mut self, ev: ExecCommandBeginEvent) {
        self.flush_answer_stream_with_separator();
        if is_exec_source(ev.source) {
            self.track_exec_process_begin(&ev);
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

    pub(crate) fn on_exec_command_output_delta(&mut self, ev: ExecCommandOutputDeltaEvent) {
        self.track_exec_output_chunk(&ev.call_id, &ev.chunk);
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

    pub(crate) fn on_terminal_interaction(&mut self, ev: TerminalInteractionEvent) {
        if !self.bottom_pane.is_task_running() {
            return;
        }
        self.flush_answer_stream_with_separator();
        let command_display = self
            .exec_processes
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
            match &mut self.exec_wait_streak {
                Some(wait) if wait.process_id == ev.process_id => {
                    wait.update_command_display(command_display);
                }
                Some(_) => {
                    self.flush_exec_wait_streak();
                    self.exec_wait_streak =
                        Some(ExecWaitStreak::new(ev.process_id, command_display));
                }
                None => {
                    self.exec_wait_streak =
                        Some(ExecWaitStreak::new(ev.process_id, command_display));
                }
            }
            self.request_redraw();
        } else {
            if self
                .exec_wait_streak
                .as_ref()
                .is_some_and(|wait| wait.process_id == ev.process_id)
            {
                self.flush_exec_wait_streak();
            }
            self.add_to_history(history_cell::new_exec_interaction(
                command_display,
                ev.stdin,
            ));
        }
    }

    pub(crate) fn on_patch_apply_begin(&mut self, event: PatchApplyBeginEvent) {
        self.add_to_history(history_cell::new_patch_event(
            event.changes,
            &self.config.cwd,
        ));
    }

    pub(crate) fn on_patch_apply_end(&mut self, event: chaos_ipc::protocol::PatchApplyEndEvent) {
        let ev2 = event.clone();
        self.defer_or_handle(
            |q| q.push_patch_end(event),
            |s| s.handle_patch_apply_end_now(ev2),
        );
    }

    pub(crate) fn on_exec_command_end(&mut self, ev: ExecCommandEndEvent) {
        if is_exec_source(ev.source) {
            if let Some(process_id) = ev.process_id.as_deref()
                && self
                    .exec_wait_streak
                    .as_ref()
                    .is_some_and(|wait| wait.process_id == process_id)
            {
                self.flush_exec_wait_streak();
            }
            self.track_exec_process_end(&ev);
            if !self.bottom_pane.is_task_running() {
                return;
            }
        }
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_exec_end(ev), |s| s.handle_exec_end_now(ev2));
    }

    pub(crate) fn track_exec_process_begin(&mut self, ev: &ExecCommandBeginEvent) {
        if ev.source != ExecCommandSource::ExecStartup {
            return;
        }
        let key = ev.process_id.clone().unwrap_or(ev.call_id.to_string());
        let command_display = strip_bash_lc_and_escape(&ev.command);
        if let Some(existing) = self
            .exec_processes
            .iter_mut()
            .find(|process| process.key == key)
        {
            existing.call_id = ev.call_id.clone();
            existing.command_display = command_display;
            existing.recent_chunks.clear();
        } else {
            self.exec_processes.push(ExecProcessSummary {
                key,
                call_id: ev.call_id.clone(),
                command_display,
                recent_chunks: Vec::new(),
            });
        }
        self.sync_exec_footer();
    }

    pub(crate) fn track_exec_process_end(&mut self, ev: &ExecCommandEndEvent) {
        let key = ev.process_id.clone().unwrap_or(ev.call_id.to_string());
        let before = self.exec_processes.len();
        self.exec_processes.retain(|process| process.key != key);
        if self.exec_processes.len() != before {
            self.sync_exec_footer();
        }
    }

    pub(crate) fn sync_exec_footer(&mut self) {
        let processes = self
            .exec_processes
            .iter()
            .map(|process| process.command_display.clone())
            .collect();
        self.bottom_pane.set_exec_processes(processes);
    }

    pub(crate) fn track_exec_output_chunk(&mut self, call_id: &str, chunk: &[u8]) {
        let Some(process) = self
            .exec_processes
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
}
