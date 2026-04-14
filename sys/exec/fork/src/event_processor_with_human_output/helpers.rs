use chaos_ipc::protocol::EventMsg;
use chaos_uptime::format_duration;
use std::io::Write;
use std::time::Duration;

use super::AgentJobProgressMessage;
use super::EventProcessorWithHumanOutput;

pub(super) struct AgentJobProgressStats {
    pub(super) processed: usize,
    pub(super) total: usize,
    pub(super) percent: i64,
    pub(super) failed: usize,
    pub(super) running: usize,
    pub(super) pending: usize,
}

pub(super) fn should_print_final_message_to_stdout(
    final_message: Option<&str>,
    stdout_is_terminal: bool,
    stderr_is_terminal: bool,
) -> bool {
    final_message.is_some() && !(stdout_is_terminal && stderr_is_terminal)
}

pub(super) fn format_agent_job_progress_line(
    columns: Option<usize>,
    job_label: &str,
    stats: AgentJobProgressStats,
    eta: &str,
) -> String {
    let rest = format!(
        "{processed}/{total} {percent}% f{failed} r{running} p{pending} eta {eta}",
        processed = stats.processed,
        total = stats.total,
        percent = stats.percent,
        failed = stats.failed,
        running = stats.running,
        pending = stats.pending
    );
    let prefix = format!("job {job_label}");
    let base_len = prefix.len() + rest.len() + 4;
    let mut bar_width = columns
        .and_then(|columns| columns.checked_sub(base_len))
        .filter(|available| *available > 0)
        .unwrap_or(20usize);
    let with_bar = |width: usize| {
        let filled = ((stats.processed as f64 / stats.total as f64) * width as f64)
            .round()
            .clamp(0.0, width as f64) as usize;
        let mut bar = "#".repeat(filled);
        bar.push_str(&"-".repeat(width - filled));
        format!("{prefix} [{bar}] {rest}")
    };
    let mut line = with_bar(bar_width);
    if let Some(columns) = columns
        && line.len() > columns
    {
        let min_line = format!("{prefix} {rest}");
        if min_line.len() > columns {
            let mut truncated = min_line;
            if columns > 2 && truncated.len() > columns {
                truncated.truncate(columns - 2);
                truncated.push_str("..");
            }
            return truncated;
        }
        let available = columns.saturating_sub(base_len);
        if available == 0 {
            return min_line;
        }
        bar_width = available.min(bar_width).max(1);
        line = with_bar(bar_width);
    }
    line
}

impl EventProcessorWithHumanOutput {
    pub(super) fn parse_agent_job_progress(message: &str) -> Option<AgentJobProgressMessage> {
        let payload = message.strip_prefix("agent_job_progress:")?;
        serde_json::from_str::<AgentJobProgressMessage>(payload).ok()
    }

    pub(super) fn is_silent_event(msg: &EventMsg) -> bool {
        match msg {
            EventMsg::HookStarted(event) => {
                !EventProcessorWithHumanOutput::should_print_hook_started(event)
            }
            EventMsg::HookCompleted(event) => {
                !EventProcessorWithHumanOutput::should_print_hook_completed(event)
            }
            _ => matches!(
                msg,
                EventMsg::ProcessNameUpdated(_)
                    | EventMsg::TokenCount(_)
                    | EventMsg::TurnStarted(_)
                    | EventMsg::ExecApprovalRequest(_)
                    | EventMsg::ApplyPatchApprovalRequest(_)
                    | EventMsg::TerminalInteraction(_)
                    | EventMsg::ExecCommandOutputDelta(_)
                    | EventMsg::GetHistoryEntryResponse(_)
                    | EventMsg::McpListToolsResponse(_)
                    | EventMsg::ListCustomPromptsResponse(_)
                    | EventMsg::RawResponseItem(_)
                    | EventMsg::UserMessage(_)
                    | EventMsg::EnteredReviewMode(_)
                    | EventMsg::ExitedReviewMode(_)
                    | EventMsg::ItemStarted(_)
                    | EventMsg::ItemCompleted(_)
                    | EventMsg::AgentMessageContentDelta(_)
                    | EventMsg::PlanDelta(_)
                    | EventMsg::ReasoningContentDelta(_)
                    | EventMsg::ReasoningRawContentDelta(_)
                    | EventMsg::UndoCompleted(_)
                    | EventMsg::UndoStarted(_)
                    | EventMsg::ProcessRolledBack(_)
                    | EventMsg::RequestUserInput(_)
                    | EventMsg::RequestPermissions(_)
                    | EventMsg::ElicitationComplete(_)
                    | EventMsg::DynamicToolCallRequest(_)
                    | EventMsg::DynamicToolCallResponse(_)
            ),
        }
    }

    pub(super) fn should_interrupt_progress(msg: &EventMsg) -> bool {
        if let EventMsg::HookCompleted(event) = msg {
            return EventProcessorWithHumanOutput::should_print_hook_completed(event);
        }
        matches!(
            msg,
            EventMsg::Error(_)
                | EventMsg::Warning(_)
                | EventMsg::DeprecationNotice(_)
                | EventMsg::StreamError(_)
                | EventMsg::TurnComplete(_)
                | EventMsg::ShutdownComplete
        )
    }

    pub(super) fn finish_progress_line(&mut self) {
        if self.progress_active {
            self.progress_active = false;
            self.progress_last_len = 0;
            self.progress_done = false;
            if self.use_ansi_cursor {
                if self.progress_anchor {
                    eprintln!("\u{1b}[1A\u{1b}[1G\u{1b}[2K");
                } else {
                    eprintln!("\u{1b}[1G\u{1b}[2K");
                }
            } else {
                eprintln!();
            }
            self.progress_anchor = false;
        }
    }

    pub(super) fn render_agent_job_progress(&mut self, update: AgentJobProgressMessage) {
        let total = update.total_items.max(1);
        let processed = update.completed_items + update.failed_items;
        let percent = (processed as f64 / total as f64 * 100.0).round() as i64;
        let job_label = update.job_id.chars().take(8).collect::<String>();
        let eta = update
            .eta_seconds
            .map(|secs| format_duration(Duration::from_secs(secs)))
            .unwrap_or_else(|| "--".to_string());
        let columns = std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0);
        let line = format_agent_job_progress_line(
            columns,
            job_label.as_str(),
            AgentJobProgressStats {
                processed,
                total,
                percent,
                failed: update.failed_items,
                running: update.running_items,
                pending: update.pending_items,
            },
            eta.as_str(),
        );
        let done = processed >= update.total_items;
        if !self.use_ansi_cursor {
            eprintln!("{line}");
            if done {
                self.progress_active = false;
                self.progress_last_len = 0;
            }
            return;
        }
        if done && self.progress_done {
            return;
        }
        if !self.progress_active {
            eprintln!();
            self.progress_anchor = true;
            self.progress_done = false;
        }
        let mut output = String::new();
        if self.progress_anchor {
            output.push_str("\u{1b}[1A\u{1b}[1G\u{1b}[2K");
        } else {
            output.push_str("\u{1b}[1G\u{1b}[2K");
        }
        output.push_str(&line);
        if done {
            output.push('\n');
            eprint!("{output}");
            self.progress_active = false;
            self.progress_last_len = 0;
            self.progress_anchor = false;
            self.progress_done = true;
            return;
        }
        eprint!("{output}");
        let _ = std::io::stderr().flush();
        self.progress_active = true;
        self.progress_last_len = line.len();
    }
}
