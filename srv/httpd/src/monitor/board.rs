//! The resident as the monitor shows it, folded from the activity bus. Holds
//! only what the bus carries: names, counts, outcomes and timing.

use std::collections::{BTreeMap, HashMap, VecDeque};

use chaos_ipc::ProcessId;
pub(super) use chaos_kern::activity::fleet::Origin;
use chaos_kern::activity::{
    Activity, ActivityEvent, AgentRun, Attention, CallOutcome, Check, CheckKind, LimitWindow,
    MachineCause, MachinePhase, ModelUsage, Station, TurnOutcome,
};

pub(super) const FEED_LEN: usize = 100;

/// The stations in the order the map shows them.
pub(super) const STATIONS: [Station; 9] = [
    Station::Kernel,
    Station::Agents,
    Station::Shell,
    Station::Files,
    Station::Network,
    Station::Mcp,
    Station::Web,
    Station::Edit,
    Station::Hooks,
];
const SESSIONS_LEN: usize = 64;
const ROUTE_LEN: usize = 7;

#[derive(Debug, Default)]
pub(super) struct Board {
    pub sessions: HashMap<ProcessId, Session>,
    pub machine: Option<(MachinePhase, Vec<MachineCause>)>,
    pub limits: Vec<LimitWindow>,
    pub stations: HashMap<Station, Tally>,
    pub feed: VecDeque<Entry>,
    /// The last stations visited, oldest first, repeats collapsed.
    pub route: VecDeque<(Station, i64)>,
    pub turn_ended: Option<(i64, TurnOutcome)>,
    /// Other Chaos processes of this home forwarding their activity here.
    pub elsewhere: Vec<Origin>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Tally {
    pub done: u64,
    pub failed: u64,
    pub failed_at_ms: Option<i64>,
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
    /// Calls that failed one after another, reset by one that succeeds.
    pub failing: u32,
    /// A check kind whose last run failed, until a run of that kind passes.
    pub failed_check: Option<CheckKind>,
    /// Set when this session is a minion: who dispatched it, and as what.
    pub parent: Option<ProcessId>,
    pub nickname: Option<String>,
    pub role: Option<String>,
    /// The minions this session dispatched, in the order it did.
    pub minions: Vec<Minion>,
    /// Minions this session is blocked on, and since when.
    pub waiting_on: Vec<ProcessId>,
    pub waiting_since_ms: Option<i64>,
    /// Its parent closed it; it will do no more work.
    pub closed: bool,
    /// The process it runs in, when that is not this resident.
    pub origin: Option<Origin>,
}

#[derive(Debug, Clone)]
pub(super) struct Minion {
    pub id: ProcessId,
    pub nickname: Option<String>,
    pub role: Option<String>,
    pub state: AgentRun,
}

#[derive(Debug, Clone)]
pub(super) struct Call {
    pub station: Station,
    pub label: String,
    pub check: Option<CheckKind>,
    pub started_ms: i64,
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
    Map,
    Feed,
    /// A process of this home started or stopped forwarding.
    Fleet,
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
            failing: 0,
            failed_check: None,
            parent: None,
            nickname: None,
            role: None,
            minions: Vec::new(),
            waiting_on: Vec::new(),
            waiting_since_ms: None,
            closed: false,
            origin: None,
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
        let entry = fold(session, &event.activity, event.at_ms);
        let mut changes = vec![if added {
            Change::SessionAdded
        } else {
            Change::Session(id)
        }];
        // After the session exists, so a parent's first event can be a spawn.
        changes.extend(self.relate(id, event));
        match &event.activity {
            Activity::CallStarted { station, .. } => {
                if self.route.back().map(|(last, _)| last) != Some(station) {
                    self.route.push_back((*station, event.at_ms));
                    while self.route.len() > ROUTE_LEN {
                        self.route.pop_front();
                    }
                }
                changes.push(Change::Map);
            }
            Activity::CallEnded {
                station, outcome, ..
            } => {
                let tally = self.stations.entry(*station).or_default();
                if *outcome == CallOutcome::Ok {
                    tally.done += 1;
                } else {
                    tally.failed += 1;
                    tally.failed_at_ms = Some(event.at_ms);
                }
                changes.push(Change::Map);
            }
            Activity::TurnEnded { outcome } => {
                self.turn_ended = Some((event.at_ms, *outcome));
                changes.push(Change::Map);
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

    /// An event from another process of this home.
    pub(super) fn apply_remote(&mut self, origin: Origin, event: &ActivityEvent) -> Vec<Change> {
        let changes = self.apply(event);
        if let Some(session) = self.sessions.get_mut(&event.process_id) {
            session.origin = Some(origin);
        }
        changes
    }

    pub(super) fn joined(&mut self, origin: Origin) -> Vec<Change> {
        if !self.elsewhere.contains(&origin) {
            self.elsewhere.push(origin);
        }
        vec![Change::Fleet]
    }

    /// A process exited: its sessions can do no more work, so they go.
    pub(super) fn left(&mut self, origin: Origin) -> Vec<Change> {
        self.elsewhere.retain(|o| *o != origin);
        self.sessions
            .retain(|_, session| session.origin != Some(origin));
        vec![Change::Fleet, Change::SessionAdded, Change::Map]
    }

    /// Events that tie a session to its minions touch both sides.
    fn relate(&mut self, id: ProcessId, event: &ActivityEvent) -> Vec<Change> {
        let at = event.at_ms;
        match &event.activity {
            Activity::AgentSpawned {
                child,
                nickname,
                role,
            } => {
                let added = !self.sessions.contains_key(child);
                if added {
                    self.make_room();
                }
                let minion = self
                    .sessions
                    .entry(*child)
                    .or_insert_with(|| Session::new(at));
                minion.parent = Some(id);
                minion.nickname = nickname.clone();
                minion.role = role.clone();
                if let Some(parent) = self.sessions.get_mut(&id)
                    && !parent.minions.iter().any(|m| m.id == *child)
                {
                    parent.minions.push(Minion {
                        id: *child,
                        nickname: nickname.clone(),
                        role: role.clone(),
                        state: AgentRun::Starting,
                    });
                }
                vec![if added {
                    Change::SessionAdded
                } else {
                    Change::Session(*child)
                }]
            }
            Activity::AgentState { child, state } => {
                self.set_minion(id, *child, |minion| minion.state = *state);
                vec![Change::Session(*child), Change::Map]
            }
            Activity::AgentEnded { child } => {
                self.set_minion(id, *child, |minion| {
                    if matches!(minion.state, AgentRun::Starting | AgentRun::Running) {
                        minion.state = AgentRun::Stopped;
                    }
                });
                if let Some(minion) = self.sessions.get_mut(child) {
                    minion.closed = true;
                }
                vec![Change::Session(*child)]
            }
            _ => Vec::new(),
        }
    }

    fn set_minion(
        &mut self,
        parent: ProcessId,
        child: ProcessId,
        update: impl FnOnce(&mut Minion),
    ) {
        if let Some(minion) = self
            .sessions
            .get_mut(&parent)
            .and_then(|parent| parent.minions.iter_mut().find(|m| m.id == child))
        {
            update(minion);
        }
    }

    /// The name a minion goes by, else its short id.
    pub(super) fn label(&self, id: &ProcessId) -> String {
        self.sessions
            .get(id)
            .and_then(|session| session.nickname.clone())
            .unwrap_or_else(|| {
                let id = id.to_string();
                id[id.len().saturating_sub(4)..].to_string()
            })
    }

    /// Forget the longest-idle session that is not working.
    fn make_room(&mut self) {
        if self.sessions.len() < SESSIONS_LEN {
            return;
        }
        let idle = self
            .sessions
            .iter()
            .filter(|(_, session)| !session.working && session.waiting_on.is_empty())
            .min_by_key(|(_, session)| session.last_at_ms)
            .map(|(id, _)| *id);
        if let Some(id) = idle {
            self.sessions.remove(&id);
        }
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

mod fold;
use fold::fold;

#[cfg(test)]
mod tests;
