//! The fleet bus over Postgres, for processes on any machine that share a
//! database: each NOTIFYs its activity, and residents LISTEN.
//!
//! A notification carries the origin and one event, or none as a heartbeat;
//! a resident takes an origin as gone once its heartbeats are overdue by
//! their own measured rhythm, or after `SILENCE` while that is still unknown.
//! A process notifies only while some resident listens, so a fleet nobody
//! watches costs the database nothing.

use std::collections::HashMap;
use std::time::Duration;

use chrono_machines::Exponential;
use chrono_machines::Retry;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use suspect_machines::{Config, Distribution, Normal, Roster, Suspect, Threshold};
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;

use super::{FleetEvent, Origin};
use crate::activity::ActivityEvent;

const CHANNEL: &str = "chaos_activity";
const LISTEN: &str = "LISTEN \"chaos_activity\"";
const HEARTBEAT: Duration = Duration::from_secs(10);
/// How often a resident proves its own listening connection still works.
const PROBE: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(30)
};
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const RECONNECT: Exponential = match Exponential::new(RECONNECT_MIN, 2.0, RECONNECT_MAX, 0.0) {
    Some(shape) => shape,
    None => panic!("reconnect backoff shape is invalid"),
};
const SILENCE: Duration = Duration::from_secs(30);
const JITTER: Duration = Duration::from_secs(1);
const WARM_UP: usize = 3;
const WINDOW: usize = 16;
const PEERS: usize = 256;
const SUSPECTS: usize = 32;
const GONE: Threshold = match Threshold::new(8.0) {
    Some(threshold) => threshold,
    None => panic!("the departure threshold is invalid"),
};
const POLICY: Config = {
    let Ok(normal) = Normal::new(JITTER) else {
        panic!("the jitter floor is invalid")
    };
    let Ok(config) = Config::new(Distribution::Normal(normal), WARM_UP) else {
        panic!("the warm-up is invalid")
    };
    let Ok(config) = config.with_acceptable_pause(HEARTBEAT) else {
        panic!("the acceptable pause is invalid")
    };
    let Ok(config) = config.with_warm_up_ceiling(SILENCE) else {
        panic!("the warm-up ceiling is invalid")
    };
    config
};
static ROSTER: Roster<PEERS, WINDOW> = match Roster::new(POLICY) {
    Ok(roster) => roster,
    Err(_) => panic!("the warm-up does not fit the window"),
};
const HEARD: u64 = 0;
const BEATING: u64 = 1;
/// Postgres refuses payloads of 8000 bytes or more.
const MAX_PAYLOAD: usize = 7900;

#[derive(Debug, Serialize, Deserialize)]
struct Notice {
    origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<ActivityEvent>,
}

struct Presence {
    roster: Roster<PEERS, WINDOW>,
    ids: HashMap<Origin, u64>,
    origins: HashMap<u64, Origin>,
    next: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum Heard {
    Joined,
    Present,
    Unheard,
}

impl Presence {
    fn new() -> Self {
        Self {
            roster: ROSTER.clone(),
            ids: HashMap::new(),
            origins: HashMap::new(),
            next: 0,
        }
    }

    fn heard(&mut self, origin: &Origin, heartbeat: bool, now: Duration) -> Heard {
        let known = self.ids.get(origin).copied();
        let id = known.unwrap_or(self.next);
        let beat = match (heartbeat, known) {
            (true, _) => self.roster.heartbeat(id, BEATING, now),
            (false, None) => self.roster.heartbeat(id, HEARD, now),
            (false, Some(_)) => return Heard::Present,
        };
        match (known, beat) {
            (Some(_), _) => Heard::Present,
            (None, Ok(_)) => {
                self.ids.insert(origin.clone(), id);
                self.origins.insert(id, origin.clone());
                self.next += 1;
                Heard::Joined
            }
            (None, Err(refusal)) => {
                tracing::warn!(?origin, %refusal, "fleet origin not tracked");
                Heard::Unheard
            }
        }
    }

    fn sweep(&mut self, now: Duration) -> Vec<Origin> {
        let mut overdue = Vec::new();
        let mut found = [Suspect::default(); SUSPECTS];
        let mut cursor = Some(0);
        while let Some(from) = cursor {
            let Ok(scan) = self.roster.scan(now, GONE, from, &mut found) else {
                break;
            };
            overdue.extend(found.iter().take(scan.suspects()).map(Suspect::peer));
            cursor = scan.next();
        }
        let mut gone = Vec::with_capacity(overdue.len());
        for id in overdue {
            _ = self.roster.forget(id);
            if let Some(origin) = self.origins.remove(&id) {
                self.ids.remove(&origin);
                gone.push(origin);
            }
        }
        gone
    }
}

/// Whether `storage_url` names a Postgres database, so residents can listen.
pub fn shared(storage_url: &str) -> bool {
    storage_url.starts_with("postgres://") || storage_url.starts_with("postgresql://")
}

/// Whether any session on this database is listening for activity. Postgres
/// shows only what the role may see, so a failure to tell counts as yes.
async fn watched(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
         WHERE datname = current_database() AND query ILIKE 'LISTEN %chaos_activity%')",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(true)
}

async fn notify(pool: &PgPool, notice: &Notice) {
    let Ok(payload) = serde_json::to_string(notice) else {
        return;
    };
    if payload.len() < MAX_PAYLOAD {
        let _ = sqlx::query("SELECT pg_notify($1, $2)")
            .bind(CHANNEL)
            .bind(payload)
            .execute(pool)
            .await;
    }
}

pub(super) async fn forward(pool: PgPool, origin: Origin) {
    let mut bus: Option<broadcast::Receiver<ActivityEvent>> = None;
    let mut beat = tokio::time::interval(HEARTBEAT);
    loop {
        tokio::select! {
            _ = beat.tick() => {
                match (watched(&pool).await, bus.is_some()) {
                    (true, false) => bus = Some(crate::activity::subscribe()),
                    (false, true) => bus = None,
                    _ => {}
                }
                if bus.is_some() {
                    notify(&pool, &Notice { origin: origin.clone(), event: None }).await;
                    // A resident may have started since the last beat.
                    for event in crate::activity::open_sessions() {
                        notify(&pool, &Notice { origin: origin.clone(), event: Some(event) }).await;
                    }
                }
            }
            event = async { bus.as_mut()?.recv().await.ok() }, if bus.is_some() => {
                if let Some(event) = event {
                    notify(&pool, &Notice { origin: origin.clone(), event: Some(event) }).await;
                }
            }
        }
    }
}

/// Listen for every process notifying this database. Its own connection, so
/// the shared pool keeps all of its slots.
pub async fn listen(storage_url: &str) -> Result<mpsc::Receiver<FleetEvent>, sqlx::Error> {
    let mut presence = Presence::new();
    let mut listener = subscribe(storage_url).await?;
    let storage_url = storage_url.to_string();
    let (tx, rx) = mpsc::channel(1024);
    tokio::spawn(async move {
        let epoch = Instant::now();
        let mut sweep = tokio::time::interval(HEARTBEAT);
        let mut probe = tokio::time::interval_at(Instant::now() + PROBE, PROBE);
        loop {
            let heard = tokio::select! {
                notification = listener.recv() => match notification {
                    Ok(notification) => serde_json::from_str::<Notice>(notification.payload()).ok(),
                    // PgListener recovers from some lost connections but not a
                    // reset one, so any failure starts over on a new connection.
                    Err(_) => {
                        listener = resubscribe(&storage_url).await;
                        None
                    }
                },
                // A connection the server dropped without a word looks idle
                // forever; asking it something is the only way to know. Ask
                // with LISTEN again: a no-op, but it stays the statement other
                // processes look for to tell that a resident is listening.
                _ = probe.tick() => {
                    let alive = tokio::time::timeout(
                        PROBE_TIMEOUT,
                        sqlx::query(LISTEN).execute(&mut listener),
                    )
                    .await;
                    if !matches!(alive, Ok(Ok(_))) {
                        listener = resubscribe(&storage_url).await;
                    }
                    None
                }
                _ = sweep.tick() => {
                    for origin in presence.sweep(epoch.elapsed()) {
                        if tx.send(FleetEvent::Left(origin)).await.is_err() {
                            return;
                        }
                    }
                    None
                }
            };
            let Some(notice) = heard else { continue };
            let heard = presence.heard(&notice.origin, notice.event.is_none(), epoch.elapsed());
            if heard == Heard::Unheard {
                continue;
            }
            if heard == Heard::Joined
                && tx
                    .send(FleetEvent::Joined(notice.origin.clone()))
                    .await
                    .is_err()
            {
                return;
            }
            if let Some(event) = notice.event
                && tx
                    .send(FleetEvent::Activity(notice.origin, event))
                    .await
                    .is_err()
            {
                return;
            }
        }
    });
    Ok(rx)
}

async fn subscribe(storage_url: &str) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect(storage_url).await?;
    listener.listen(CHANNEL).await?;
    Ok(listener)
}

/// A fresh listening connection, waiting longer between attempts while the
/// database stays out of reach.
async fn resubscribe(storage_url: &str) -> PgListener {
    let mut retry = Retry::new(RECONNECT.into(), None);
    loop {
        if let Ok(listener) = subscribe(storage_url).await {
            return listener;
        }
        let wait = retry.failed(0).unwrap_or(RECONNECT_MAX);
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests;
