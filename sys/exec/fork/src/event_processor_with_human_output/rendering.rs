use chaos_ipc::protocol::AgentStatus;
use chaos_ipc::protocol::FileChange;
use chaos_ipc::protocol::HookCompletedEvent;
use chaos_ipc::protocol::HookEventName;
use chaos_ipc::protocol::HookOutputEntryKind;
use chaos_ipc::protocol::HookRunStatus;
use chaos_ipc::protocol::HookStartedEvent;
use chaos_ipc::protocol::McpInvocation;
use owo_colors::OwoColorize;
use owo_colors::Style;
use shlex::try_join;

use super::EventProcessorWithHumanOutput;

/// Timestamped helper. The timestamp is styled with self.dimmed.
macro_rules! ts_msg {
    ($self:ident, $($arg:tt)*) => {{
        eprintln!($($arg)*);
    }};
}

impl EventProcessorWithHumanOutput {
    pub(super) fn render_hook_started(&self, event: HookStartedEvent) {
        if !Self::should_print_hook_started(&event) {
            return;
        }
        let event_name = Self::hook_event_name(event.run.event_name);
        if let Some(status_message) = event.run.status_message
            && !status_message.trim().is_empty()
        {
            ts_msg!(
                self,
                "{} {}: {}",
                "hook".style(self.magenta),
                event_name,
                status_message
            );
        }
    }

    pub(super) fn render_hook_completed(&self, event: HookCompletedEvent) {
        if !Self::should_print_hook_completed(&event) {
            return;
        }

        let event_name = Self::hook_event_name(event.run.event_name);
        let status = Self::hook_status_name(event.run.status);
        ts_msg!(
            self,
            "{} {} ({status})",
            "hook".style(self.magenta),
            event_name
        );

        for entry in event.run.entries {
            let prefix = Self::hook_entry_prefix(entry.kind);
            eprintln!("  {prefix} {}", entry.text);
        }
    }

    pub(super) fn should_print_hook_started(event: &HookStartedEvent) -> bool {
        event
            .run
            .status_message
            .as_deref()
            .is_some_and(|status_message| !status_message.trim().is_empty())
    }

    pub(super) fn should_print_hook_completed(event: &HookCompletedEvent) -> bool {
        event.run.status != HookRunStatus::Completed || !event.run.entries.is_empty()
    }

    pub(super) fn hook_event_name(event_name: HookEventName) -> &'static str {
        match event_name {
            HookEventName::SessionStart => "SessionStart",
            HookEventName::Stop => "Stop",
        }
    }

    pub(super) fn hook_status_name(status: HookRunStatus) -> &'static str {
        match status {
            HookRunStatus::Running => "running",
            HookRunStatus::Completed => "completed",
            HookRunStatus::Failed => "failed",
            HookRunStatus::Blocked => "blocked",
            HookRunStatus::Stopped => "stopped",
        }
    }

    pub(super) fn hook_entry_prefix(kind: HookOutputEntryKind) -> &'static str {
        match kind {
            HookOutputEntryKind::Warning => "warning:",
            HookOutputEntryKind::Stop => "stop:",
            HookOutputEntryKind::Feedback => "feedback:",
            HookOutputEntryKind::Context => "context:",
            HookOutputEntryKind::Error => "error:",
        }
    }
}

pub(super) fn escape_command(command: &[String]) -> String {
    try_join(command.iter().map(String::as_str)).unwrap_or_else(|_| command.join(" "))
}

pub(super) fn format_file_change(change: &FileChange) -> &'static str {
    match change {
        FileChange::Add { .. } => "A",
        FileChange::Delete { .. } => "D",
        FileChange::Update {
            move_path: Some(_), ..
        } => "R",
        FileChange::Update {
            move_path: None, ..
        } => "M",
    }
}

pub(super) fn format_collab_invocation(tool: &str, call_id: &str, prompt: Option<&str>) -> String {
    let prompt = prompt
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(|prompt| truncate_preview(prompt, /*max_chars*/ 120));
    match prompt {
        Some(prompt) => format!("{tool}({call_id}, prompt=\"{prompt}\")"),
        None => format!("{tool}({call_id})"),
    }
}

pub(super) fn format_collab_status(status: &AgentStatus) -> String {
    match status {
        AgentStatus::PendingInit => "pending init".to_string(),
        AgentStatus::Running => "running".to_string(),
        AgentStatus::Interrupted => "interrupted".to_string(),
        AgentStatus::Completed(Some(message)) => {
            let preview = truncate_preview(message.trim(), /*max_chars*/ 120);
            if preview.is_empty() {
                "completed".to_string()
            } else {
                format!("completed: \"{preview}\"")
            }
        }
        AgentStatus::Completed(None) => "completed".to_string(),
        AgentStatus::Errored(message) => {
            let preview = truncate_preview(message.trim(), /*max_chars*/ 120);
            if preview.is_empty() {
                "errored".to_string()
            } else {
                format!("errored: \"{preview}\"")
            }
        }
        AgentStatus::Shutdown => "shutdown".to_string(),
        AgentStatus::NotFound => "not found".to_string(),
    }
}

pub(super) fn style_for_agent_status(
    status: &AgentStatus,
    processor: &EventProcessorWithHumanOutput,
) -> Style {
    match status {
        AgentStatus::PendingInit | AgentStatus::Shutdown => processor.dimmed,
        AgentStatus::Running => processor.cyan,
        AgentStatus::Interrupted => processor.yellow,
        AgentStatus::Completed(_) => processor.green,
        AgentStatus::Errored(_) | AgentStatus::NotFound => processor.red,
    }
}

pub(super) fn is_collab_status_failure(status: &AgentStatus) -> bool {
    matches!(status, AgentStatus::Errored(_) | AgentStatus::NotFound)
}

pub(super) fn format_receiver_list(ids: &[chaos_ipc::ProcessId]) -> String {
    if ids.is_empty() {
        return "none".to_string();
    }
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn format_mcp_invocation(invocation: &McpInvocation) -> String {
    // Build fully-qualified tool name: server.tool
    let fq_tool_name = format!("{}.{}", invocation.server, invocation.tool);

    // Format arguments as compact JSON so they fit on one line.
    let args_str = invocation
        .arguments
        .as_ref()
        .map(|v: &serde_json::Value| serde_json::to_string(v).unwrap_or_else(|_| v.to_string()))
        .unwrap_or_default();

    if args_str.is_empty() {
        format!("{fq_tool_name}()")
    } else {
        format!("{fq_tool_name}({args_str})")
    }
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let preview = text.chars().take(max_chars).collect::<String>();
    format!("{preview}…")
}
