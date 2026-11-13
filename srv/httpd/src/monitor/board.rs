//! The resident as the monitor shows it, folded from the activity bus. Holds
//! only what the bus carries: names, counts, outcomes and timing.

use std::collections::{BTreeMap, HashMap, VecDeque};

use chaos_ipc::ProcessId;
use chaos_kern::activity::{
    Activity, ActivityEvent, Attention, CallOutcome, Check, CheckKind, LimitWindow, MachineCause,
    MachinePhase, ModelUsage, Station, TurnOutcome,
};

pub(super) const FEED_LEN: usize = 100;

/// The stations in the order the map shows them.
pub(super) const STATIONS: [Station; 8] = [
    Station::Kernel,
    Station::Shell,
    Station::Files,
    Station::Network,
    Station::Mcp,
    Station::Web,
    Station::Edit,
    Station::Hooks,
];
const SESSIONS_LEN: usize = 64;

#[derive(Debug, Default)]
pub(super) struct Board {
    pub sessions: HashMap<ProcessId, Session>,
    pub machine: Option<(MachinePhase, Vec<MachineCause>)>,
    pub limits: Vec<LimitWindow>,
    pub stations: HashMap<Station, Tally>,
    pub feed: VecDeque<Entry>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Tally {
    pub done: u64,
    pub failed: u64,
}

#[derive(Debug, Clone)]
pub(super) struct Session {
    pub working: bool,
    pub failed: bool,
    pub turns: u64,
    pub model: Option<String>,
    pub thinking: bool,
    pub context: Option<(i64, Option<i64>)>,
    pub calls: BTreeMap<String, Call>,
    pub last_check: Option<Check>,
    pub asks: BTreeMap<String, Attention>,
    pub usage: ModelUsage,
    pub agents: u64,
    pub last_at_ms: i64,
}

#[derive(Debug, Clone)]
pub(super) struct Call {
    pub station: Station,
    pub label: String,
    pub check: Option<CheckKind>,
}

/// One line of the feed: something finished, failed, or needs attention.
#[derive(Debug, Clone)]
pub(super) struct Entry {
    pub at_ms: i64,
    pub process_id: ProcessId,
    pub station: Option<Station>,
    pub text: String,
    pub tone: Tone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Quiet,
    Good,
    Bad,
    Ask,
}

/// What an event changed, so a connection re-renders only that widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Change {
    Session(ProcessId),
    SessionAdded,
    Machine,
    Limits,
    Stations,
    Feed,
}

impl Session {
    fn new(at_ms: i64) -> Self {
        Self {
            working: false,
            failed: false,
            turns: 0,
            model: None,
            thinking: false,
            context: None,
            calls: BTreeMap::new(),
            last_check: None,
            asks: BTreeMap::new(),
            usage: ModelUsage {
                input: 0,
                cache_read: 0,
                cache_write: 0,
                output: 0,
                reasoning: 0,
            },
            agents: 0,
            last_at_ms: at_ms,
        }
    }
}

impl Board {
    pub(super) fn apply(&mut self, event: &ActivityEvent) -> Vec<Change> {
        let id = event.process_id;
        match &event.activity {
            Activity::Machine { phase, causes } => {
                self.machine = Some((*phase, causes.clone()));
                return vec![Change::Machine];
            }
            Activity::Limits { windows } => {
                self.limits = windows.clone();
                return vec![Change::Limits];
            }
            _ => {}
        }
        let added = !self.sessions.contains_key(&id);
        if added {
            self.make_room();
        }
        let session = self
            .sessions
            .entry(id)
            .or_insert_with(|| Session::new(event.at_ms));
        session.last_at_ms = event.at_ms;
        let entry = fold(session, &event.activity);
        let mut changes = vec![if added {
            Change::SessionAdded
        } else {
            Change::Session(id)
        }];
        match &event.activity {
            Activity::CallStarted { .. } => changes.push(Change::Stations),
            Activity::CallEnded {
                station, outcome, ..
            } => {
                let tally = self.stations.entry(*station).or_default();
                if *outcome == CallOutcome::Ok {
                    tally.done += 1;
                } else {
                    tally.failed += 1;
                }
                changes.push(Change::Stations);
            }
            _ => {}
        }
        if let Some((station, text, tone)) = entry {
            self.feed.push_back(Entry {
                at_ms: event.at_ms,
                process_id: id,
                station,
                text,
                tone,
            });
            while self.feed.len() > FEED_LEN {
                self.feed.pop_front();
            }
            changes.push(Change::Feed);
        }
        changes
    }

    /// Forget the longest-idle session that is not working.
    fn make_room(&mut self) {
        if self.sessions.len() < SESSIONS_LEN {
            return;
        }
        let idle = self
            .sessions
            .iter()
            .filter(|(_, session)| !session.working)
            .min_by_key(|(_, session)| session.last_at_ms)
            .map(|(id, _)| *id);
        if let Some(id) = idle {
            self.sessions.remove(&id);
        }
    }
}

type Line = (Option<Station>, String, Tone);

fn fold(session: &mut Session, activity: &Activity) -> Option<Line> {
    match activity {
        Activity::TurnStarted => {
            session.working = true;
            session.failed = false;
            session.turns += 1;
            None
        }
        Activity::TurnEnded { outcome } => {
            session.working = false;
            session.thinking = false;
            session.calls.clear();
            session.asks.clear();
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
        Activity::ModelEnded { usage, .. } => {
            session.thinking = false;
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
        Activity::AgentSpawned { nickname, .. } => {
            session.agents += 1;
            let name = nickname.as_deref().unwrap_or("agent");
            Some((None, format!("spawned {name}"), Tone::Quiet))
        }
        Activity::Failed => {
            session.failed = true;
            Some((None, "error".into(), Tone::Bad))
        }
        Activity::AgentEnded { .. } | Activity::Limits { .. } | Activity::Machine { .. } => None,
    }
}

pub(super) fn attention(reason: Attention) -> &'static str {
    match reason {
        Attention::CommandApproval => "command approval",
        Attention::PatchApproval => "patch approval",
        Attention::Permissions => "permissions",
        Attention::UserInput => "input",
        Attention::Elicitation => "MCP question",
    }
}

pub(super) fn duration(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..60_000 => format!("{:.1}s", ms as f64 / 1_000.0),
        _ => format!("{}m{:02}s", ms / 60_000, ms % 60_000 / 1_000),
    }
}

#[cfg(test)]
mod tests;
