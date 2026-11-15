//! One view model per widget, built from the board.

use chaos_ipc::ProcessId;
use chaos_kern::activity::{CheckKind, MachinePhase};
use serde::Serialize;

use super::board::{self, Board, Entry as BoardEntry, Session as BoardSession, Tone};
use super::view::{Widget, name};

mod attention;
mod instruments;
mod map;

pub(super) use attention::Attention;
pub(super) use instruments::Instruments;
pub(super) use map::Map;

#[derive(Serialize)]
pub(super) struct Summary {
    pub connected: bool,
    pub version: &'static str,
    pub uptime: String,
    pub triggers: usize,
    pub max_triggers: usize,
    pub sessions: usize,
    pub working: usize,
    pub waiting: usize,
    /// Other processes whose sessions this pane also shows.
    pub elsewhere: Vec<String>,
    pub elsewhere_heading: &'static str,
}

impl Widget for Summary {
    const NAME: &'static str = "summary";
}

/// The machines this pane can switch between, and which one it shows.
#[derive(Serialize)]
pub(super) struct Machines {
    pub all: bool,
    pub hosts: Vec<Tab>,
}

#[derive(Serialize)]
pub(super) struct Tab {
    pub name: String,
    pub active: bool,
    pub here: bool,
}

impl Widget for Machines {
    const NAME: &'static str = "machines";
}

impl Machines {
    pub(super) fn from(hosts: &[String], active: Option<&str>) -> Self {
        let local = chaos_kern::activity::fleet::local_host();
        Self {
            all: active.is_none(),
            hosts: hosts
                .iter()
                .map(|host| Tab {
                    name: host.clone(),
                    active: active == Some(host.as_str()),
                    here: host == local,
                })
                .collect(),
        }
    }
}

/// Machine health: one machine's state, or every machine that is unwell.
#[derive(Serialize)]
pub(super) struct Machine {
    pub hosts: Vec<Health>,
    pub pausing: bool,
}

#[derive(Serialize)]
pub(super) struct Health {
    /// Omitted when the view already shows one machine.
    pub host: Option<String>,
    pub phase: String,
    pub tone: &'static str,
    pub causes: Vec<String>,
}

impl Widget for Machine {
    const NAME: &'static str = "machine";
}

pub(super) fn pausing(phase: &MachinePhase) -> bool {
    !matches!(phase, MachinePhase::Healthy | MachinePhase::Recovered)
}

impl Machine {
    /// `one`: the view holds a single machine, so its state is always shown;
    /// across machines only the unwell ones are worth a line.
    pub(super) fn from(board: &Board, one: bool) -> Self {
        let hosts: Vec<Health> = board
            .machine
            .iter()
            .filter(|(_, (phase, _))| one || pausing(phase))
            .map(|(host, (phase, causes))| Health {
                host: (!one).then(|| host.clone()),
                phase: name(phase),
                tone: if pausing(phase) { "bad" } else { "good" },
                causes: causes.iter().map(name).collect(),
            })
            .collect();
        Self {
            pausing: board.machine.values().any(|(phase, _)| pausing(phase)),
            hosts,
        }
    }
}

#[derive(Serialize)]
pub(super) struct Sessions {
    pub groups: Vec<Group>,
}

/// A top-level session and the minions it dispatched.
#[derive(Serialize)]
pub(super) struct Group {
    pub root: Session,
    pub minions: Vec<Session>,
}

impl Widget for Sessions {
    const NAME: &'static str = "sessions";
}

impl Sessions {
    pub(super) fn from(board: &Board) -> Self {
        // Top-level sessions by urgency, each grouped with its minions.
        let mut roots: Vec<_> = board
            .sessions
            .iter()
            .filter(|(_, session)| {
                session
                    .parent
                    .is_none_or(|parent| !board.sessions.contains_key(&parent))
            })
            .map(|(id, session)| Session::from(board, *id, session))
            .collect();
        roots.sort_by_key(|session| (session.rank, std::cmp::Reverse(session.last_at_ms)));
        let groups = roots
            .into_iter()
            .map(|root| {
                let minions = board
                    .sessions
                    .get(&root.process_id)
                    .map(|session| session.minions.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|minion| {
                        let session = board.sessions.get(&minion.id)?;
                        Some(Session::from(board, minion.id, session))
                    })
                    .collect();
                Group { root, minions }
            })
            .collect();
        Self { groups }
    }
}

#[derive(Serialize)]
pub(super) struct Session {
    #[serde(skip)]
    pub process_id: ProcessId,
    pub id: String,
    pub short: String,
    pub state: &'static str,
    pub tone: &'static str,
    pub hue: usize,
    #[serde(skip)]
    pub rank: u8,
    pub last_at_ms: i64,
    pub turns: u64,
    pub model: Option<String>,
    pub thinking: bool,
    pub context_percent: Option<u8>,
    pub calls: Vec<Call>,
    pub check: Option<Check>,
    pub asks: Vec<&'static str>,
    pub tokens_in: String,
    pub tokens_out: String,
    pub cache_percent: Option<u8>,
    pub agents: u64,
    /// Set on a minion: what it is called and who dispatched it.
    pub minion_of: Option<Parent>,
    pub minions: Vec<MinionChip>,
    pub waiting_on: usize,
    pub waiting_for: Option<String>,
    /// Where it runs, when that is not this resident.
    pub origin: Option<String>,
}

#[derive(Serialize)]
pub(super) struct Parent {
    pub name: Option<String>,
    pub role: Option<String>,
    pub parent: String,
    pub parent_hue: usize,
}

#[derive(Serialize)]
pub(super) struct MinionChip {
    pub name: String,
    pub role: Option<String>,
    pub hue: usize,
    pub state: String,
}

#[derive(Serialize)]
pub(super) struct Call {
    pub station: String,
    pub label: String,
    pub check: Option<String>,
}

#[derive(Serialize)]
pub(super) struct Check {
    pub kind: String,
    pub passed: Option<u64>,
    pub failed: Option<u64>,
    pub skipped: Option<u64>,
    pub ok: bool,
}

impl Widget for Session {
    const NAME: &'static str = "session";
}

impl Session {
    pub(super) fn from(board: &Board, process_id: ProcessId, session: &BoardSession) -> Self {
        let id = process_id.to_string();
        let (state, tone, rank) = if !session.asks.is_empty() {
            ("waiting", "ask", 0)
        } else if !session.waiting_on.is_empty() {
            ("delegating", "busy", 1)
        } else if session.exited_at_ms.is_some() {
            ("exited", "quiet", 5)
        } else if session.closed {
            ("closed", "quiet", 4)
        } else if session.working {
            ("working", "busy", 1)
        } else if session.failed {
            ("failed", "bad", 2)
        } else {
            ("idle", "quiet", 3)
        };
        let usage = &session.usage;
        let fed = usage.input.max(0);
        Self {
            process_id,
            short: short(&id),
            id,
            state,
            tone,
            hue: map::hue(&process_id),
            rank,
            last_at_ms: session.last_at_ms,
            turns: session.turns,
            model: session.model.clone(),
            thinking: session.thinking,
            context_percent: session.context.and_then(|(used, window)| {
                let window = window.filter(|window| *window > 0)?;
                Some(((used.max(0) * 100) / window).min(100) as u8)
            }),
            calls: session
                .calls
                .values()
                .map(|call| Call {
                    station: name(&call.station),
                    label: call.label.clone(),
                    check: call.check.map(|kind: CheckKind| name(&kind)),
                })
                .collect(),
            check: session.last_check.map(|check| {
                let counts = check.counts;
                Check {
                    kind: name(&check.kind),
                    passed: counts.map(|c| c.passed),
                    failed: counts.map(|c| c.failed),
                    skipped: counts.map(|c| c.skipped).filter(|n| *n > 0),
                    ok: counts.is_none_or(|c| c.failed == 0),
                }
            }),
            asks: session
                .asks
                .values()
                .map(|reason| board::attention(*reason))
                .collect(),
            tokens_in: compact(fed),
            tokens_out: compact(usage.output.max(0)),
            cache_percent: (fed > 0)
                .then(|| ((usage.cache_read.max(0) * 100) / fed).min(100) as u8),
            agents: session.agents,
            minion_of: session.parent.map(|parent| Parent {
                name: session.nickname.clone(),
                role: session.role.clone().filter(|role| role != "default"),
                parent: board.label(&parent),
                parent_hue: map::hue(&parent),
            }),
            minions: session
                .minions
                .iter()
                .map(|minion| MinionChip {
                    name: minion
                        .nickname
                        .clone()
                        .unwrap_or_else(|| board.label(&minion.id)),
                    role: minion.role.clone().filter(|role| role != "default"),
                    hue: map::hue(&minion.id),
                    state: name(&minion.state),
                })
                .collect(),
            origin: session.origin.as_ref().map(origin_label),
            waiting_on: session.waiting_on.len(),
            waiting_for: session.waiting_since_ms.map(|since| {
                board::duration(u64::try_from(session.last_at_ms.max(since) - since).unwrap_or(0))
            }),
        }
    }
}

#[derive(Serialize)]
pub(super) struct Feed {
    pub entries: Vec<Entry>,
}

impl Widget for Feed {
    const NAME: &'static str = "feed";
}

impl Feed {
    pub(super) fn from(board: &Board) -> Self {
        Self {
            entries: board
                .feed
                .iter()
                .rev()
                .map(|entry| Entry::from(board, entry))
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct Entry {
    pub time: String,
    pub session: String,
    pub hue: usize,
    pub station: Option<String>,
    pub text: String,
    pub tone: &'static str,
}

impl Widget for Entry {
    const NAME: &'static str = "entry";
}

impl Entry {
    pub(super) fn from(board: &Board, entry: &BoardEntry) -> Self {
        let id = entry.process_id.to_string();
        Self {
            time: jiff::Timestamp::from_millisecond(entry.at_ms)
                .map(|at| at.strftime("%H:%M:%S").to_string())
                .unwrap_or_default(),
            session: board
                .sessions
                .get(&entry.process_id)
                .and_then(|session| session.nickname.clone())
                .unwrap_or_else(|| format!("…{}", short(&id))),
            hue: map::hue(&entry.process_id),
            station: entry.station.as_ref().map(name),
            text: entry.text.clone(),
            tone: match entry.tone {
                Tone::Quiet => "quiet",
                Tone::Good => "good",
                Tone::Bad => "bad",
                Tone::Ask => "ask",
            },
        }
    }
}

#[derive(Serialize)]
pub(super) struct Dashboard {
    pub machines: Machines,
    pub summary: Summary,
    pub machine: Machine,
    pub instruments: Instruments,
    pub map: Map,
    pub attention: Attention,
    pub sessions: Sessions,
    pub feed: Feed,
}

impl Widget for Dashboard {
    const NAME: &'static str = "dashboard";
}

/// `console 4242`: what kind of process, and which one.
pub(super) fn process_label(origin: &board::Origin) -> String {
    format!("{} {}", name(&origin.kind), origin.pid)
}

/// `console 4242`, and `@ silica` when it runs on another machine.
pub(super) fn origin_label(origin: &board::Origin) -> String {
    let label = process_label(origin);
    if origin.host == chaos_kern::activity::fleet::local_host() {
        label
    } else {
        format!("{label} @ {}", origin.host)
    }
}

/// The last four characters: enough to tell live sessions apart.
pub(super) fn short(id: &str) -> String {
    let start = id.char_indices().rev().nth(3).map_or(0, |(at, _)| at);
    id[start..].to_string()
}

pub(super) fn compact(count: i64) -> String {
    match count {
        ..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{:.1}k", count as f64 / 1_000.0),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}
