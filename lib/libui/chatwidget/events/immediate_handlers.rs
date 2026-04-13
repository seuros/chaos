//! Immediate-mode exec/MCP/patch handlers called by `InterruptManager::drain_queue`.

use chaos_ipc::protocol::ExecCommandBeginEvent;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::ExecCommandSource;
use chaos_ipc::protocol::McpToolCallBeginEvent;
use chaos_ipc::protocol::McpToolCallEndEvent;

use crate::app_event::AppEvent;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::ExecCell;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell;
use crate::history_cell::McpToolCallCell;

use super::super::ChatWidget;
use super::super::core::RunningCommand;
use super::super::core::UnifiedExecWaitState;

impl ChatWidget {
    pub fn handle_exec_end_now(&mut self, ev: ExecCommandEndEvent) {
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

    pub fn handle_exec_begin_now(&mut self, ev: ExecCommandBeginEvent) {
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
