//! What every session in this OS process is doing, redacted for live monitoring.
//!
//! One bus per OS process, like the global tracer: a resident runs several
//! process tables (serve's own, two per MCP session) and the monitor watches
//! all of them. Events carry names, counts, outcomes and timing only; never
//! prompts, arguments, command lines past the program name, output or file
//! contents. Publishing costs nothing while nobody subscribes.

use std::sync::LazyLock;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecCommandStatus;
use chaos_ipc::protocol::HookRunStatus;
use chaos_ipc::protocol::PatchApplyStatus;
use serde::Serialize;
use tokio::sync::broadcast;

mod command;

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
    for activity in Activity::from_event(msg) {
        send(process_id, activity);
    }
}

/// A `needs_you` ask was answered. An ask whose turn ends unanswered is
/// closed by that `turn_ended`, not by this.
pub(crate) fn resolved(process_id: ProcessId, ask_id: String) {
    if BUS.receiver_count() > 0 {
        send(process_id, Activity::Resolved { ask_id });
    }
}

/// The ask id of an MCP server's elicitation, as `needs_you` reports it.
pub(crate) fn elicitation_ask_id(server_name: &str, id: &chaos_ipc::mcp::RequestId) -> String {
    let id = serde_json::to_string(id).unwrap_or_default();
    format!("{server_name}/{id}")
}

fn send(process_id: ProcessId, activity: Activity) {
    let _ = BUS.send(ActivityEvent {
        process_id,
        at_ms: jiff::Timestamp::now().as_millisecond(),
        activity,
    });
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
    Compacted,
    Limits {
        windows: Vec<LimitWindow>,
    },
    NeedsYou {
        ask_id: String,
        reason: Attention,
    },
    Resolved {
        ask_id: String,
    },
    AgentSpawned {
        child: ProcessId,
        nickname: Option<String>,
        role: Option<String>,
    },
    AgentEnded {
        child: ProcessId,
    },
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimitWindow {
    pub used_percent: u8,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<i64>,
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
    fn from_event(msg: &EventMsg) -> Vec<Self> {
        let activity = match msg {
            EventMsg::TurnStarted(_) => Self::TurnStarted,
            EventMsg::TurnComplete(_) => Self::TurnEnded {
                outcome: TurnOutcome::Completed,
            },
            EventMsg::TurnAborted(_) => Self::TurnEnded {
                outcome: TurnOutcome::Aborted,
            },
            EventMsg::ExecCommandBegin(ev) => {
                let (station, label) = command::classify(&ev.command);
                Self::CallStarted {
                    call_id: ev.call_id.clone(),
                    station,
                    label,
                }
            }
            EventMsg::ExecCommandEnd(ev) => {
                let (station, label) = command::classify(&ev.command);
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
            EventMsg::TokenCount(ev) => return token_count(ev),
            EventMsg::ContextCompacted(_) => Self::Compacted,
            EventMsg::ExecApprovalRequest(ev) => Self::NeedsYou {
                ask_id: ev.approval_id.clone().unwrap_or_else(|| ev.call_id.clone()),
                reason: Attention::CommandApproval,
            },
            EventMsg::ApplyPatchApprovalRequest(ev) => Self::NeedsYou {
                ask_id: ev.call_id.clone(),
                reason: Attention::PatchApproval,
            },
            EventMsg::RequestPermissions(ev) => Self::NeedsYou {
                ask_id: ev.call_id.clone(),
                reason: Attention::Permissions,
            },
            EventMsg::RequestUserInput(ev) => Self::NeedsYou {
                ask_id: ev.call_id.clone(),
                reason: Attention::UserInput,
            },
            EventMsg::ElicitationRequest(ev) => Self::NeedsYou {
                ask_id: elicitation_ask_id(&ev.server_name, &ev.id),
                reason: Attention::Elicitation,
            },
            EventMsg::CollabAgentSpawnEnd(ev) => match ev.new_process_id {
                Some(child) => Self::AgentSpawned {
                    child,
                    nickname: ev.new_agent_nickname.clone(),
                    role: ev.new_agent_role.clone(),
                },
                None => return Vec::new(),
            },
            EventMsg::CollabCloseEnd(ev) => Self::AgentEnded {
                child: ev.receiver_process_id,
            },
            EventMsg::Error(_) => Self::Failed,
            _ => return Vec::new(),
        };
        vec![activity]
    }
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

fn token_count(ev: &chaos_ipc::protocol::TokenCountEvent) -> Vec<Activity> {
    let mut activity = Vec::new();
    if let Some(info) = &ev.info {
        activity.push(Activity::Context {
            used_tokens: info.last_token_usage.tokens_in_context_window(),
            window: info.model_context_window,
        });
    }
    if let Some(limits) = &ev.rate_limits {
        let windows: Vec<LimitWindow> = [&limits.primary, &limits.secondary]
            .into_iter()
            .flatten()
            .map(|window| LimitWindow {
                used_percent: window.used_percent.clamp(0.0, 100.0).round() as u8,
                window_minutes: window.window_minutes,
                resets_at: window.resets_at,
            })
            .collect();
        if !windows.is_empty() {
            activity.push(Activity::Limits { windows });
        }
    }
    activity
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
