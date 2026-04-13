mod helpers;
mod processor;
mod rendering;

use chaos_kern::config::Config;
use owo_colors::Style;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

/// This should be configurable. When used in CI, users may not want to impose
/// a limit so they can see the full transcript.
const MAX_OUTPUT_LINES_FOR_EXEC_TOOL_CALL: usize = 20;

pub(crate) struct EventProcessorWithHumanOutput {
    call_id_to_patch: HashMap<String, PatchApplyBegin>,

    // To ensure that --color=never is respected, ANSI escapes _must_ be added
    // using .style() with one of these fields. If you need a new style, add a
    // new field here.
    bold: Style,
    italic: Style,
    dimmed: Style,

    magenta: Style,
    red: Style,
    green: Style,
    cyan: Style,
    yellow: Style,
    bold_yellow: Style,

    /// Whether to include `AgentReasoning` events in the output.
    show_agent_reasoning: bool,
    show_raw_agent_reasoning: bool,
    last_message_path: Option<PathBuf>,
    last_total_token_usage: Option<chaos_ipc::protocol::TokenUsageInfo>,
    final_message: Option<String>,
    last_proposed_plan: Option<String>,
    progress_active: bool,
    progress_last_len: usize,
    use_ansi_cursor: bool,
    progress_anchor: bool,
    progress_done: bool,
}

impl EventProcessorWithHumanOutput {
    pub(crate) fn create_with_ansi(
        with_ansi: bool,
        cursor_ansi: bool,
        config: &Config,
        last_message_path: Option<PathBuf>,
    ) -> Self {
        let call_id_to_patch = HashMap::new();

        if with_ansi {
            Self {
                call_id_to_patch,
                bold: Style::new().bold(),
                italic: Style::new().italic(),
                dimmed: Style::new().dimmed(),
                magenta: Style::new().magenta(),
                red: Style::new().red(),
                green: Style::new().green(),
                cyan: Style::new().cyan(),
                yellow: Style::new().yellow(),
                bold_yellow: Style::new().bold().yellow(),
                show_agent_reasoning: !config.hide_agent_reasoning,
                show_raw_agent_reasoning: config.show_raw_agent_reasoning,
                last_message_path,
                last_total_token_usage: None,
                final_message: None,
                last_proposed_plan: None,
                progress_active: false,
                progress_last_len: 0,
                use_ansi_cursor: cursor_ansi,
                progress_anchor: false,
                progress_done: false,
            }
        } else {
            Self {
                call_id_to_patch,
                bold: Style::new(),
                italic: Style::new(),
                dimmed: Style::new(),
                magenta: Style::new(),
                red: Style::new(),
                green: Style::new(),
                cyan: Style::new(),
                yellow: Style::new(),
                bold_yellow: Style::new(),
                show_agent_reasoning: !config.hide_agent_reasoning,
                show_raw_agent_reasoning: config.show_raw_agent_reasoning,
                last_message_path,
                last_total_token_usage: None,
                final_message: None,
                last_proposed_plan: None,
                progress_active: false,
                progress_last_len: 0,
                use_ansi_cursor: cursor_ansi,
                progress_anchor: false,
                progress_done: false,
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct AgentJobProgressMessage {
    pub(super) job_id: String,
    pub(super) total_items: usize,
    pub(super) pending_items: usize,
    pub(super) running_items: usize,
    pub(super) completed_items: usize,
    pub(super) failed_items: usize,
    pub(super) eta_seconds: Option<u64>,
}

pub(super) struct PatchApplyBegin {
    pub(super) start_time: Instant,
    pub(super) auto_approved: bool,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chaos_ipc::protocol::EventMsg;
    use chaos_ipc::protocol::HookCompletedEvent;
    use chaos_ipc::protocol::HookEventName;
    use chaos_ipc::protocol::HookExecutionMode;
    use chaos_ipc::protocol::HookHandlerType;
    use chaos_ipc::protocol::HookOutputEntry;
    use chaos_ipc::protocol::HookRunStatus;
    use chaos_ipc::protocol::HookRunSummary;
    use chaos_ipc::protocol::HookScope;
    use chaos_ipc::protocol::HookStartedEvent;

    use super::EventProcessorWithHumanOutput;
    use super::helpers::should_print_final_message_to_stdout;
    use pretty_assertions::assert_eq;

    #[test]
    fn suppresses_final_stdout_message_when_both_streams_are_terminals() {
        assert_eq!(
            should_print_final_message_to_stdout(Some("hello"), true, true),
            false
        );
    }

    #[test]
    fn prints_final_stdout_message_when_stdout_is_not_terminal() {
        assert_eq!(
            should_print_final_message_to_stdout(Some("hello"), false, true),
            true
        );
    }

    #[test]
    fn prints_final_stdout_message_when_stderr_is_not_terminal() {
        assert_eq!(
            should_print_final_message_to_stdout(Some("hello"), true, false),
            true
        );
    }

    #[test]
    fn does_not_print_when_message_is_missing() {
        assert_eq!(
            should_print_final_message_to_stdout(None, false, false),
            false
        );
    }

    #[test]
    fn hook_started_with_status_message_is_not_silent() {
        let event = HookStartedEvent {
            turn_id: Some("turn-1".to_string()),
            run: hook_run(
                HookRunStatus::Running,
                Some("running hook"),
                Vec::new(),
                HookEventName::Stop,
            ),
        };

        assert!(!EventProcessorWithHumanOutput::is_silent_event(
            &EventMsg::HookStarted(event)
        ));
    }

    #[test]
    fn hook_completed_failure_interrupts_progress() {
        let event = HookCompletedEvent {
            turn_id: Some("turn-1".to_string()),
            run: hook_run(HookRunStatus::Failed, None, Vec::new(), HookEventName::Stop),
        };

        assert!(EventProcessorWithHumanOutput::should_interrupt_progress(
            &EventMsg::HookCompleted(event)
        ));
    }

    #[test]
    fn hook_completed_success_without_entries_stays_silent() {
        let event = HookCompletedEvent {
            turn_id: Some("turn-1".to_string()),
            run: hook_run(
                HookRunStatus::Completed,
                None,
                Vec::new(),
                HookEventName::Stop,
            ),
        };

        assert!(EventProcessorWithHumanOutput::is_silent_event(
            &EventMsg::HookCompleted(event)
        ));
    }

    fn hook_run(
        status: HookRunStatus,
        status_message: Option<&str>,
        entries: Vec<HookOutputEntry>,
        event_name: HookEventName,
    ) -> HookRunSummary {
        HookRunSummary {
            id: "hook-run-1".to_string(),
            event_name,
            handler_type: HookHandlerType::Command,
            execution_mode: HookExecutionMode::Sync,
            scope: HookScope::Turn,
            source_path: PathBuf::from("/tmp/hooks.json"),
            display_order: 0,
            status,
            status_message: status_message.map(ToOwned::to_owned),
            started_at: 0,
            completed_at: Some(1),
            duration_ms: Some(1),
            entries,
        }
    }
}
