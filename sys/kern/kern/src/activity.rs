//! What every session in this OS process is doing, redacted for live monitoring.
//!
//! One bus per OS process, like the global tracer: a resident runs several
//! process tables (serve's own, two per MCP session) and the monitor watches
//! all of them. Events carry names, counts, outcomes and timing only; never
//! prompts, arguments, command lines past the program name, output or file
//! contents. Publishing costs nothing while nobody subscribes.

use std::path::Path;
use std::sync::LazyLock;

use chaos_ipc::ProcessId;
use chaos_ipc::parse_command::ParsedCommand;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecCommandStatus;
use chaos_ipc::protocol::HookRunStatus;
use chaos_ipc::protocol::PatchApplyStatus;
use serde::Serialize;
use tokio::sync::broadcast;

const CAPACITY: usize = 1024;

static BUS: LazyLock<broadcast::Sender<ActivityEvent>> =
    LazyLock::new(|| broadcast::channel(CAPACITY).0);

/// Every session's activity from now on. A slow subscriber lags rather than
/// slowing a session.
pub fn subscribe() -> broadcast::Receiver<ActivityEvent> {
    BUS.subscribe()
}

pub(crate) fn publish(process_id: ProcessId, msg: &EventMsg) {
    if BUS.receiver_count() == 0 {
        return;
    }
    if let Some(activity) = Activity::from_event(msg) {
        let _ = BUS.send(ActivityEvent {
            process_id,
            at_ms: jiff::Timestamp::now().as_millisecond(),
            activity,
        });
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityEvent {
    pub process_id: ProcessId,
    pub at_ms: i64,
    #[serde(flatten)]
    pub activity: Activity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Activity {
    TurnStarted,
    TurnEnded {
        outcome: TurnOutcome,
    },
    CallStarted {
        call_id: String,
        station: Station,
        label: String,
    },
    CallEnded {
        call_id: String,
        station: Station,
        label: String,
        outcome: CallOutcome,
        duration_ms: Option<u64>,
    },
    Context {
        used_tokens: i64,
        window: Option<i64>,
    },
    NeedsYou {
        reason: Attention,
    },
    AgentSpawned {
        child: ProcessId,
        nickname: Option<String>,
        role: Option<String>,
    },
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Station {
    Shell,
    Files,
    Network,
    Mcp,
    Web,
    Edit,
    Hooks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    Ok,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    CommandApproval,
    PatchApproval,
    Permissions,
    UserInput,
    Elicitation,
}

impl Activity {
    fn from_event(msg: &EventMsg) -> Option<Self> {
        Some(match msg {
            EventMsg::TurnStarted(_) => Self::TurnStarted,
            EventMsg::TurnComplete(_) => Self::TurnEnded {
                outcome: TurnOutcome::Completed,
            },
            EventMsg::TurnAborted(_) => Self::TurnEnded {
                outcome: TurnOutcome::Aborted,
            },
            EventMsg::ExecCommandBegin(ev) => {
                let (station, label) = command(&ev.command, &ev.parsed_cmd);
                Self::CallStarted {
                    call_id: ev.call_id.clone(),
                    station,
                    label,
                }
            }
            EventMsg::ExecCommandEnd(ev) => {
                let (station, label) = command(&ev.command, &ev.parsed_cmd);
                Self::CallEnded {
                    call_id: ev.call_id.clone(),
                    station,
                    label,
                    outcome: match ev.status {
                        ExecCommandStatus::Completed => CallOutcome::Ok,
                        ExecCommandStatus::Failed => CallOutcome::Failed,
                        ExecCommandStatus::Declined => CallOutcome::Declined,
                    },
                    duration_ms: Some(millis(ev.duration)),
                }
            }
            EventMsg::McpToolCallBegin(ev) => Self::CallStarted {
                call_id: ev.call_id.clone(),
                station: Station::Mcp,
                label: mcp_label(ev.invocation.server.as_deref(), &ev.invocation.tool),
            },
            EventMsg::McpToolCallEnd(ev) => Self::CallEnded {
                call_id: ev.call_id.clone(),
                station: Station::Mcp,
                label: mcp_label(ev.invocation.server.as_deref(), &ev.invocation.tool),
                outcome: match &ev.result {
                    Ok(result) if result.is_error != Some(true) => CallOutcome::Ok,
                    _ => CallOutcome::Failed,
                },
                duration_ms: Some(millis(ev.duration)),
            },
            EventMsg::WebSearchBegin(ev) => Self::CallStarted {
                call_id: ev.call_id.clone(),
                station: Station::Web,
                label: "web search".to_string(),
            },
            EventMsg::WebSearchEnd(ev) => Self::CallEnded {
                call_id: ev.call_id.clone(),
                station: Station::Web,
                label: "web search".to_string(),
                outcome: CallOutcome::Ok,
                duration_ms: None,
            },
            EventMsg::PatchApplyBegin(ev) => Self::CallStarted {
                call_id: ev.call_id.clone(),
                station: Station::Edit,
                label: files_label(ev.changes.len()),
            },
            EventMsg::PatchApplyEnd(ev) => Self::CallEnded {
                call_id: ev.call_id.clone(),
                station: Station::Edit,
                label: files_label(ev.changes.len()),
                outcome: match ev.status {
                    PatchApplyStatus::Completed => CallOutcome::Ok,
                    PatchApplyStatus::Failed => CallOutcome::Failed,
                    PatchApplyStatus::Declined => CallOutcome::Declined,
                },
                duration_ms: None,
            },
            EventMsg::HookStarted(ev) => Self::CallStarted {
                call_id: ev.run.id.clone(),
                station: Station::Hooks,
                label: hook_label(ev.run.event_name),
            },
            EventMsg::HookCompleted(ev) => Self::CallEnded {
                call_id: ev.run.id.clone(),
                station: Station::Hooks,
                label: hook_label(ev.run.event_name),
                outcome: match ev.run.status {
                    HookRunStatus::Running | HookRunStatus::Completed => CallOutcome::Ok,
                    HookRunStatus::Failed | HookRunStatus::Stopped => CallOutcome::Failed,
                    HookRunStatus::Blocked => CallOutcome::Declined,
                },
                duration_ms: ev.run.duration_ms.and_then(|ms| u64::try_from(ms).ok()),
            },
            EventMsg::TokenCount(ev) => {
                let info = ev.info.as_ref()?;
                Self::Context {
                    used_tokens: info.last_token_usage.tokens_in_context_window(),
                    window: info.model_context_window,
                }
            }
            EventMsg::ExecApprovalRequest(_) => Self::NeedsYou {
                reason: Attention::CommandApproval,
            },
            EventMsg::ApplyPatchApprovalRequest(_) => Self::NeedsYou {
                reason: Attention::PatchApproval,
            },
            EventMsg::RequestPermissions(_) => Self::NeedsYou {
                reason: Attention::Permissions,
            },
            EventMsg::RequestUserInput(_) => Self::NeedsYou {
                reason: Attention::UserInput,
            },
            EventMsg::ElicitationRequest(_) => Self::NeedsYou {
                reason: Attention::Elicitation,
            },
            EventMsg::CollabAgentSpawnEnd(ev) => Self::AgentSpawned {
                child: ev.new_process_id?,
                nickname: ev.new_agent_nickname.clone(),
                role: ev.new_agent_role.clone(),
            },
            EventMsg::Error(_) => Self::Failed,
            _ => return None,
        })
    }
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "fish"];
const NETWORK: &[&str] = &[
    "curl", "wget", "gh", "ssh", "scp", "sftp", "rsync", "nc", "http", "xh",
];
const GIT_NETWORK: &[&str] = &["push", "pull", "fetch", "clone", "ls-remote"];

/// The station and the program name; nothing past it.
fn command(argv: &[String], parsed: &[ParsedCommand]) -> (Station, String) {
    let words = run_words(argv, parsed);
    let program = words
        .first()
        .map(|word| program_name(word))
        .unwrap_or("command");
    let files_only = !parsed.is_empty()
        && parsed
            .iter()
            .all(|cmd| !matches!(cmd, ParsedCommand::Unknown { .. }));
    let station = if files_only {
        Station::Files
    } else if NETWORK.contains(&program)
        || (program == "git"
            && words
                .get(1)
                .is_some_and(|sub| GIT_NETWORK.contains(&sub.as_str())))
    {
        Station::Network
    } else {
        Station::Shell
    };
    (station, program.to_string())
}

/// The words of the command actually run: a `sh -c` wrapper is looked
/// through to the first command of its script.
fn run_words(argv: &[String], parsed: &[ParsedCommand]) -> Vec<String> {
    let wrapped = argv
        .first()
        .is_some_and(|first| SHELLS.contains(&program_name(first)))
        && argv
            .iter()
            .any(|arg| arg.starts_with('-') && arg.contains('c'));
    if !wrapped {
        return argv.iter().take(2).cloned().collect();
    }
    let script = match parsed.first() {
        Some(
            ParsedCommand::Read { cmd, .. }
            | ParsedCommand::ListFiles { cmd, .. }
            | ParsedCommand::Search { cmd, .. }
            | ParsedCommand::Unknown { cmd },
        ) => cmd.as_str(),
        None => argv.last().map_or("", String::as_str),
    };
    script
        .split_whitespace()
        .take(2)
        .map(str::to_string)
        .collect()
}

fn program_name(word: &str) -> &str {
    Path::new(word)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(word)
}

fn mcp_label(server: Option<&str>, tool: &str) -> String {
    match server {
        Some(server) => format!("{server}.{tool}"),
        None => tool.to_string(),
    }
}

fn files_label(count: usize) -> String {
    match count {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    }
}

fn hook_label(event: chaos_ipc::protocol::HookEventName) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "hook".to_string())
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
