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
}

impl Widget for Summary {
    const NAME: &'static str = "summary";
}

#[derive(Serialize)]
pub(super) struct Machine {
    pub phase: String,
    pub tone: &'static str,
    pub causes: Vec<String>,
    pub pausing: bool,
}

impl Widget for Machine {
    const NAME: &'static str = "machine";
}

impl Machine {
    pub(super) fn from(board: &Board) -> Self {
        match &board.machine {
            Some((phase, causes)) => {
                let pausing = !matches!(phase, MachinePhase::Healthy | MachinePhase::Recovered);
                Self {
                    phase: name(phase),
                    tone: if pausing { "bad" } else { "good" },
                    causes: causes.iter().map(name).collect(),
                    pausing,
                }
            }
            // Sessions report only changes; a machine that never warned says nothing.
            None => Self {
                phase: "no warnings".into(),
                tone: "quiet",
                causes: Vec::new(),
                pausing: false,
            },
        }
    }
}

#[derive(Serialize)]
pub(super) struct Sessions {
    pub items: Vec<Session>,
}

impl Widget for Sessions {
    const NAME: &'static str = "sessions";
}

impl Sessions {
    pub(super) fn from(board: &Board) -> Self {
        let mut items: Vec<_> = board
            .sessions
            .iter()
            .map(|(id, session)| Session::from(*id, session))
            .collect();
        items.sort_by_key(|session| (session.rank, std::cmp::Reverse(session.last_at_ms)));
        Self { items }
    }
}

#[derive(Serialize)]
pub(super) struct Session {
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
    pub(super) fn from(process_id: ProcessId, session: &BoardSession) -> Self {
        let id = process_id.to_string();
        let (state, tone, rank) = if !session.asks.is_empty() {
            ("waiting", "ask", 0)
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
            entries: board.feed.iter().rev().map(Entry::from).collect(),
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

impl From<&BoardEntry> for Entry {
    fn from(entry: &BoardEntry) -> Self {
        let id = entry.process_id.to_string();
        Self {
            time: jiff::Timestamp::from_millisecond(entry.at_ms)
                .map(|at| at.strftime("%H:%M:%S").to_string())
                .unwrap_or_default(),
            session: short(&id),
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
