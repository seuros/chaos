use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rama::futures::async_stream::stream_fn;
use rama::http::Body;
use rama::http::HeaderValue;
use rama::http::Response;
use rama::http::StatusCode;
use rama::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use rama::http::service::web::response::{DatastarScript, IntoResponse, Sse};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::ServerState;
use board::{Board, Change};
use view::{Patch, prepend, replace};
use widgets::{
    Attention, Dashboard, Entry, Feed, Instruments, Machine, Map, Session, Sessions, Summary,
};

mod board;
mod view;
mod widgets;

const MONITOR_EVENT_CAPACITY: usize = 256;
const CHANGE_CAPACITY: usize = 1024;
const TICK: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MonitorEvent {
    pub at_ms: u128,
    pub kind: MonitorEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MonitorEventKind {
    ServerStarted,
    TriggerAccepted,
    ProcessStarted,
    TriggerCompleted,
    TriggerFailed,
    TriggerTimedOut,
    ProcessCleanedUp,
}

pub(crate) struct MonitorState {
    started_at: Instant,
    tx: broadcast::Sender<MonitorEvent>,
    board: Arc<Mutex<Board>>,
    changes: broadcast::Sender<Change>,
}

impl std::fmt::Debug for MonitorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MonitorState").finish_non_exhaustive()
    }
}

impl MonitorState {
    pub(crate) fn new() -> Self {
        let (tx, _rx) = broadcast::channel(MONITOR_EVENT_CAPACITY);
        let (changes, _rx) = broadcast::channel(CHANGE_CAPACITY);
        Self {
            started_at: Instant::now(),
            tx,
            board: Arc::default(),
            changes,
        }
    }

    /// Fold every session's activity into the board from now on, so a
    /// monitor that connects later still sees what is running.
    pub(crate) fn follow_activity(&self) {
        let board = self.board.clone();
        let changes = self.changes.clone();
        let mut activity = chaos_kern::activity::subscribe();
        tokio::spawn(async move {
            loop {
                match activity.recv().await {
                    Ok(event) => {
                        let applied = match board.lock() {
                            Ok(mut board) => board.apply(&event),
                            Err(_) => break,
                        };
                        for change in applied {
                            let _ = changes.send(change);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = changes.send(Change::SessionAdded);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    pub(crate) fn publish(
        &self,
        kind: MonitorEventKind,
        conversation_id: Option<String>,
        process_id: Option<String>,
        detail: Option<String>,
    ) {
        let _ = self.tx.send(MonitorEvent {
            at_ms: self.started_at.elapsed().as_millis(),
            kind,
            conversation_id,
            process_id,
            detail,
        });
    }

    fn subscribe(&self) -> broadcast::Receiver<MonitorEvent> {
        self.tx.subscribe()
    }

    fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }
}

pub(crate) fn page_response() -> Response {
    match view::page() {
        Ok(html) => html_response(html),
        Err(error) => {
            chaos_snitch::tracing::error!(%error, "monitor page does not render");
            let mut response = Response::new(Body::from("monitor unavailable"));
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        }
    }
}

fn html_response(html: String) -> Response {
    let mut response = Response::new(Body::from(html));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Public like the page: it is the same for every resident of this build.
pub(crate) fn stylesheet_response(if_none_match: Option<&HeaderValue>) -> Response {
    let sheet = &*view::STYLESHEET;
    let etag = format!("\"{}\"", sheet.version);
    let mut response = if if_none_match.is_some_and(|tag| tag.as_bytes() == etag.as_bytes()) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        response
    } else {
        Response::new(Body::from(sheet.css.clone()))
    };
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    if let Ok(etag) = HeaderValue::from_str(&etag) {
        headers.insert(ETAG, etag);
    }
    response
}

pub(crate) const STYLESHEET_PATH: &str = "/monitor/app.css";
pub(crate) const IF_NONE_MATCH_HEADER: rama::http::HeaderName = IF_NONE_MATCH;

pub(crate) fn events_response(state: Arc<ServerState>) -> Response {
    let mut monitor = state.monitor.subscribe();
    let mut changes = state.monitor.changes.subscribe();

    let stream = stream_fn(async move |mut yielder| {
        yielder.yield_item(dashboard(&state)).await;
        let mut clock = tokio::time::interval(TICK);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        clock.tick().await;
        loop {
            let patches = tokio::select! {
                _ = clock.tick() => tick(&state),
                event = monitor.recv() => match event {
                    Ok(_) => vec![replace(&summary(&state))],
                    Err(broadcast::error::RecvError::Lagged(_)) => vec![dashboard(&state)],
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                change = changes.recv() => match change {
                    Ok(change) => patch(&state, &change),
                    Err(broadcast::error::RecvError::Lagged(_)) => vec![dashboard(&state)],
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            };
            for patch in patches {
                yielder.yield_item(patch).await;
            }
        }
    });

    let mut response = Sse::new(stream).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

pub(crate) fn datastar_script_response() -> Response {
    DatastarScript::default().into_response()
}

fn board(state: &ServerState) -> Result<std::sync::MutexGuard<'_, Board>, rama::error::BoxError> {
    state
        .monitor
        .board
        .lock()
        .map_err(|_| "monitor board poisoned".into())
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn dashboard(state: &ServerState) -> Patch {
    let now = now_ms();
    let widget = {
        let board = board(state)?;
        Dashboard {
            summary: summary_from(state, &board),
            machine: Machine::from(&board),
            instruments: Instruments::from(&board, now),
            map: Map::from(&board, now),
            attention: Attention::from(&board, now),
            sessions: Sessions::from(&board),
            feed: Feed::from(&board),
        }
    };
    replace(&widget)
}

fn summary(state: &ServerState) -> Summary {
    match board(state) {
        Ok(board) => summary_from(state, &board),
        Err(_) => summary_from(state, &Board::default()),
    }
}

fn summary_from(state: &ServerState, board: &Board) -> Summary {
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
    }
}

/// Re-render only what changed; a new session reshapes the grid.
fn patch(state: &ServerState, change: &Change) -> Vec<Patch> {
    let board = match board(state) {
        Ok(board) => board,
        Err(error) => return vec![Err(error)],
    };
    let now = now_ms();
    match change {
        Change::Session(id) => vec![
            match board.sessions.get(id) {
                Some(session) => replace(&Session::from(*id, session)),
                None => replace(&Sessions::from(&board)),
            },
            replace(&Instruments::from(&board, now)),
            replace(&Attention::from(&board, now)),
        ],
        Change::SessionAdded => vec![
            replace(&summary_from(state, &board)),
            replace(&Sessions::from(&board)),
            replace(&Map::from(&board, now)),
        ],
        Change::Machine => vec![
            replace(&Machine::from(&board)),
            replace(&Attention::from(&board, now)),
        ],
        Change::Limits => vec![replace(&Instruments::from(&board, now))],
        Change::Map => vec![replace(&Map::from(&board, now))],
        Change::Feed => vec![match board.feed.back() {
            Some(entry) if board.feed.len() > 1 && board.feed.len() < board::FEED_LEN => {
                prepend("#w-feed-list", &Entry::from(entry))
            }
            _ => replace(&Feed::from(&board)),
        }],
    }
}

/// Clocks move without events: uptime, slow calls, gauge pace.
fn tick(state: &ServerState) -> Vec<Patch> {
    let board = match board(state) {
        Ok(board) => board,
        Err(error) => return vec![Err(error)],
    };
    let now = now_ms();
    vec![
        replace(&summary_from(state, &board)),
        replace(&Attention::from(&board, now)),
        replace(&Instruments::from(&board, now)),
    ]
}

#[cfg(test)]
mod tests;
