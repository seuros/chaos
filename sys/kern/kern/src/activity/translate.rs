//! How a session event reads on the bus: names, counts and outcomes only.

use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecCommandStatus;
use chaos_ipc::protocol::HookRunStatus;
use chaos_ipc::protocol::PatchApplyStatus;

use super::Activity;
use super::Attention;
use super::CallOutcome;
use super::Check;
use super::LimitWindow;
use super::Station;
use super::TurnOutcome;
use super::check;
use super::command;
use super::elicitation_ask_id;

impl Activity {
    pub(super) fn from_event(msg: &EventMsg) -> Vec<Self> {
        let activity = match msg {
            EventMsg::TurnStarted(_) => Self::TurnStarted,
            EventMsg::TurnComplete(_) => Self::TurnEnded {
                outcome: TurnOutcome::Completed,
            },
            EventMsg::TurnAborted(_) => Self::TurnEnded {
                outcome: TurnOutcome::Aborted,
            },
            EventMsg::ExecCommandBegin(ev) => {
                let command = command::classify(&ev.command);
                Self::CallStarted {
                    call_id: ev.call_id.clone(),
                    station: command.station,
                    label: command.label,
                    check: command.check,
                }
            }
            EventMsg::ExecCommandEnd(ev) => {
                let command = command::classify(&ev.command);
                Self::CallEnded {
                    call_id: ev.call_id.clone(),
                    station: command.station,
                    label: command.label,
                    check: command.check.map(|kind| Check {
                        kind,
                        counts: check::counts(&ev.aggregated_output),
                    }),
                    outcome: match ev.status {
                        ExecCommandStatus::Completed => CallOutcome::Ok,
                        ExecCommandStatus::Failed => CallOutcome::Failed,
                        ExecCommandStatus::Declined => CallOutcome::Declined,
                    },
                    duration_ms: Some(millis(ev.duration)),
                }
            }
            EventMsg::McpToolCallBegin(ev) => Self::CallStarted {
                check: None,
                call_id: ev.call_id.clone(),
                station: match mcp_station(&ev.invocation) {
                    Some(station) => station,
                    None => return Vec::new(),
                },
                label: mcp_label(ev.invocation.server.as_deref(), &ev.invocation.tool),
            },
            EventMsg::McpToolCallEnd(ev) => {
                let outcome = match &ev.result {
                    Ok(result) if result.is_error != Some(true) => CallOutcome::Ok,
                    _ => CallOutcome::Failed,
                };
                let station = match mcp_station(&ev.invocation) {
                    Some(station) => station,
                    // A command that never started has no exec events of its own.
                    None if outcome == CallOutcome::Failed => Station::Shell,
                    None => return Vec::new(),
                };
                Self::CallEnded {
                    check: None,
                    call_id: ev.call_id.clone(),
                    station,
                    label: mcp_label(ev.invocation.server.as_deref(), &ev.invocation.tool),
                    outcome,
                    duration_ms: Some(millis(ev.duration)),
                }
            }
            EventMsg::WebSearchBegin(ev) => Self::CallStarted {
                check: None,
                call_id: ev.call_id.clone(),
                station: Station::Web,
                label: "web search".to_string(),
            },
            EventMsg::WebSearchEnd(ev) => Self::CallEnded {
                check: None,
                call_id: ev.call_id.clone(),
                station: Station::Web,
                label: "web search".to_string(),
                outcome: CallOutcome::Ok,
                duration_ms: None,
            },
            EventMsg::PatchApplyBegin(ev) => Self::CallStarted {
                check: None,
                call_id: ev.call_id.clone(),
                station: Station::Edit,
                label: files_label(ev.changes.len()),
            },
            EventMsg::PatchApplyEnd(ev) => Self::CallEnded {
                check: None,
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
                check: None,
                call_id: ev.run.id.clone(),
                station: Station::Hooks,
                label: hook_label(ev.run.event_name),
            },
            EventMsg::HookCompleted(ev) => Self::CallEnded {
                check: None,
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

/// Where a tool call belongs. Internal tools that already report through
/// their own events (commands, patches, searches) are not reported twice.
fn mcp_station(invocation: &chaos_ipc::protocol::McpInvocation) -> Option<Station> {
    if invocation.server.is_some() {
        return Some(Station::Mcp);
    }
    match invocation.tool.as_str() {
        "exec_command" | "write_stdin" | "shell" | "local_shell" | "apply_patch" | "web_search" => {
            None
        }
        "read_file" | "list_dir" | "grep_files" | "view_image" => Some(Station::Files),
        _ => Some(Station::Kernel),
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

pub(super) fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
