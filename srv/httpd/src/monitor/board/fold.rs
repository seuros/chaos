//! How one event changes the session it belongs to.

use chaos_kern::activity::{Activity, AgentRun, CallOutcome, Station, TurnOutcome};

use super::{Call, Session, Tone, attention, duration};

type Line = (Option<Station>, String, Tone);

pub(super) fn fold(session: &mut Session, activity: &Activity, at_ms: i64) -> Option<Line> {
    match activity {
        Activity::TurnStarted => {
            session.working = true;
            session.failed = false;
            session.failing = 0;
            session.turns += 1;
            None
        }
        Activity::TurnEnded { outcome } => {
            session.working = false;
            session.thinking = false;
            session.calls.clear();
            session.asks.clear();
            session.waiting_on.clear();
            session.waiting_since_ms = None;
            Some(match outcome {
                TurnOutcome::Completed => (None, "turn completed".into(), Tone::Good),
                TurnOutcome::Aborted => (None, "turn aborted".into(), Tone::Bad),
            })
        }
        Activity::CallStarted {
            call_id,
            station,
            label,
            check,
        } => {
            session.calls.insert(
                call_id.clone(),
                Call {
                    station: *station,
                    label: label.clone(),
                    check: *check,
                    started_ms: at_ms,
                },
            );
            None
        }
        Activity::CallEnded {
            call_id,
            station,
            label,
            outcome,
            duration_ms,
            check,
        } => {
            session.calls.remove(call_id);
            if check.is_some() {
                session.last_check = *check;
            }
            let failed = *outcome != CallOutcome::Ok
                || check
                    .and_then(|check| check.counts)
                    .is_some_and(|counts| counts.failed > 0);
            session.failing = if failed { session.failing + 1 } else { 0 };
            if let Some(check) = check {
                if failed {
                    session.failed_check = Some(check.kind);
                } else if session.failed_check == Some(check.kind) {
                    session.failed_check = None;
                }
            }
            let mut text = label.clone();
            if *outcome == CallOutcome::Declined {
                text.push_str(" declined");
            } else if *outcome == CallOutcome::Failed {
                text.push_str(" failed");
            }
            if let Some(counts) = check.and_then(|check| check.counts) {
                text.push_str(&format!(
                    " · {} passed, {} failed",
                    counts.passed, counts.failed
                ));
            }
            if let Some(ms) = duration_ms {
                text.push_str(&format!(" · {}", duration(*ms)));
            }
            Some((
                Some(*station),
                text,
                if failed { Tone::Bad } else { Tone::Quiet },
            ))
        }
        Activity::Context {
            used_tokens,
            window,
        } => {
            session.context = Some((*used_tokens, *window));
            None
        }
        Activity::ModelStarted { model, .. } => {
            session.model = Some(model.clone());
            session.thinking = true;
            None
        }
        Activity::ModelEnded { model, usage, .. } => {
            session.thinking = false;
            session.model = Some(model.clone());
            if let Some(usage) = usage {
                session.usage.input += usage.input;
                session.usage.cache_read += usage.cache_read;
                session.usage.cache_write += usage.cache_write;
                session.usage.output += usage.output;
                session.usage.reasoning += usage.reasoning;
            }
            None
        }
        Activity::Compacted => Some((None, "context compacted".into(), Tone::Quiet)),
        Activity::NeedsYou { ask_id, reason } => {
            session.asks.insert(ask_id.clone(), *reason);
            Some((
                None,
                format!("needs you: {}", attention(*reason)),
                Tone::Ask,
            ))
        }
        Activity::Resolved { ask_id } => {
            session.asks.remove(ask_id);
            None
        }
        Activity::AgentSpawned { nickname, role, .. } => {
            session.agents += 1;
            let name = nickname.as_deref().unwrap_or("a minion");
            let text = match role {
                Some(role) => format!("dispatched {name} as {role}"),
                None => format!("dispatched {name}"),
            };
            Some((Some(Station::Agents), text, Tone::Quiet))
        }
        Activity::AgentState { state, .. } => match state {
            AgentRun::Done => Some((
                Some(Station::Agents),
                "a minion finished".into(),
                Tone::Good,
            )),
            AgentRun::Failed => Some((Some(Station::Agents), "a minion failed".into(), Tone::Bad)),
            _ => None,
        },
        Activity::WaitingOnAgents { children } => {
            session.waiting_on = children.clone();
            session.waiting_since_ms = Some(at_ms);
            let n = children.len();
            Some((
                Some(Station::Agents),
                format!("waiting on {n} minion{}", if n == 1 { "" } else { "s" }),
                Tone::Quiet,
            ))
        }
        Activity::DoneWaiting => {
            session.waiting_on.clear();
            session.waiting_since_ms = None;
            None
        }
        Activity::Failed => {
            session.failed = true;
            Some((None, "error".into(), Tone::Bad))
        }
        Activity::AgentEnded { .. }
        | Activity::AgentMessaged { .. }
        | Activity::Limits { .. }
        | Activity::Machine { .. } => None,
    }
}
