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
pub(super) struct MinionJobProgressMessage {
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
mod tests;
