//! What each connection is sent: the dashboard, then only what changed, for
//! every machine or the one it watches.

use super::board::{self, Board, Change};
use super::view::{Patch, prepend, replace};
use super::widgets::{
    self, Attention, Dashboard, Entry, Feed, Instruments, Machine, Machines, Map, Session,
    Sessions, Summary,
};
use super::{board as lock, now_ms};
use crate::ServerState;

/// The board this connection watches: every machine, or one.
pub(super) fn view(
    state: &ServerState,
    host: &Option<String>,
) -> Result<Board, rama::error::BoxError> {
    let board = lock(state)?;
    Ok(match host {
        Some(host) => board.only(host),
        None => board.clone(),
    })
}

fn machines(state: &ServerState, host: &Option<String>) -> Result<Machines, rama::error::BoxError> {
    Ok(Machines::from(&lock(state)?.hosts(), host.as_deref()))
}

pub(super) fn dashboard(state: &ServerState, host: &Option<String>) -> Patch {
    let now = now_ms();
    let board = view(state, host)?;
    replace(&Dashboard {
        machines: machines(state, host)?,
        summary: summary_from(state, &board, host),
        machine: Machine::from(&board),
        instruments: Instruments::from(&board, now),
        map: Map::from(&board, now),
        attention: Attention::from(&board, now),
        sessions: Sessions::from(&board),
        feed: Feed::from(&board),
    })
}

/// `host`: the machine this view holds, so its processes need no `@ host`.
pub(super) fn summary_from(state: &ServerState, board: &Board, host: &Option<String>) -> Summary {
    let max = state.max_concurrent;
    let sessions: Vec<_> = board.sessions.values().collect();
    Summary {
        connected: true,
        version: chaos_ipc::product::CHAOS_VERSION,
        uptime: board::duration(
            u64::try_from(state.monitor.uptime().as_millis()).unwrap_or(u64::MAX),
        ),
        triggers: max.saturating_sub(state.semaphore.available_permits()),
        max_triggers: max,
        sessions: sessions.len(),
        working: sessions.iter().filter(|session| session.working).count(),
        waiting: sessions
            .iter()
            .filter(|session| !session.asks.is_empty())
            .count(),
        elsewhere: board
            .elsewhere
            .iter()
            .map(|origin| match host {
                Some(_) => widgets::process_label(origin),
                None => widgets::origin_label(origin),
            })
            .collect(),
        elsewhere_heading: if host.is_some() {
            "processes"
        } else {
            "also watching"
        },
    }
}

/// Re-render only what changed, and only what this connection watches.
pub(super) fn patch(state: &ServerState, change: &Change, host: &Option<String>) -> Vec<Patch> {
    let board = match view(state, host) {
        Ok(board) => board,
        Err(error) => return vec![Err(error)],
    };
    let now = now_ms();
    match change {
        Change::Session(id) => match board.sessions.get(id) {
            Some(session) => vec![
                replace(&Session::from(&board, *id, session)),
                replace(&Instruments::from(&board, now)),
                replace(&Attention::from(&board, now)),
            ],
            // Another machine's session, or one just gone.
            None if host.is_some() => Vec::new(),
            None => vec![replace(&Sessions::from(&board))],
        },
        Change::SessionAdded => {
            let mut patches = vec![
                replace(&summary_from(state, &board, host)),
                replace(&Sessions::from(&board)),
                replace(&Map::from(&board, now)),
            ];
            if let Ok(machines) = machines(state, host) {
                patches.push(replace(&machines));
            }
            patches
        }
        Change::Machine => vec![
            replace(&Machine::from(&board)),
            replace(&Attention::from(&board, now)),
        ],
        Change::Limits => vec![replace(&Instruments::from(&board, now))],
        Change::Fleet => {
            let mut patches = vec![replace(&summary_from(state, &board, host))];
            if let Ok(machines) = machines(state, host) {
                patches.push(replace(&machines));
            }
            patches
        }
        Change::Map => vec![replace(&Map::from(&board, now))],
        Change::Feed => {
            // The newest line on the whole board: if it is another machine's,
            // this view's feed did not change.
            let newest_here = match (lock(state), host) {
                (Ok(full), Some(host)) => full.feed.back().is_some_and(|e| &e.host == host),
                _ => true,
            };
            if !newest_here {
                return Vec::new();
            }
            vec![match board.feed.back() {
                Some(entry) if board.feed.len() > 1 && board.feed.len() < board::FEED_LEN => {
                    prepend("#w-feed-list", &Entry::from(&board, entry))
                }
                _ => replace(&Feed::from(&board)),
            }]
        }
    }
}

/// Clocks move without events: uptime, slow calls, gauge pace, and exited
/// sessions ageing off the board.
pub(super) fn tick(state: &ServerState, host: &Option<String>) -> Vec<Patch> {
    let now = now_ms();
    let pruned = match lock(state) {
        Ok(mut board) => board.prune(now),
        Err(error) => return vec![Err(error)],
    };
    let board = match view(state, host) {
        Ok(board) => board,
        Err(error) => return vec![Err(error)],
    };
    let mut patches = vec![
        replace(&summary_from(state, &board, host)),
        replace(&Attention::from(&board, now)),
        replace(&Instruments::from(&board, now)),
    ];
    if pruned {
        patches.push(replace(&Sessions::from(&board)));
    }
    patches
}
