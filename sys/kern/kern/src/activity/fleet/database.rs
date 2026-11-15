//! The fleet bus over Postgres, for processes on any machine that share a
//! database: each NOTIFYs its activity, and residents LISTEN.
//!
//! A notification carries the origin and one event, or none as a heartbeat;
//! a resident that hears nothing from an origin for `SILENCE` takes it as
//! gone. A process notifies only while some resident listens, so a fleet
//! nobody watches costs the database nothing.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::postgres::PgListener;
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
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const SILENCE: Duration = Duration::from_secs(30);
/// Postgres refuses payloads of 8000 bytes or more.
const MAX_PAYLOAD: usize = 7900;

#[derive(Debug, Serialize, Deserialize)]
struct Notice {
    origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<ActivityEvent>,
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
    let mut listener = subscribe(storage_url).await?;
    let storage_url = storage_url.to_string();
    let (tx, rx) = mpsc::channel(1024);
    tokio::spawn(async move {
        let mut seen: HashMap<Origin, Instant> = HashMap::new();
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
                    let now = Instant::now();
                    let gone: Vec<Origin> = seen
                        .iter()
                        .filter(|(_, at)| now.duration_since(**at) > SILENCE)
                        .map(|(origin, _)| origin.clone())
                        .collect();
                    for origin in gone {
                        seen.remove(&origin);
                        if tx.send(FleetEvent::Left(origin)).await.is_err() {
                            return;
                        }
                    }
                    None
                }
            };
            let Some(notice) = heard else { continue };
            if seen.insert(notice.origin.clone(), Instant::now()).is_none()
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
    let mut wait = Duration::from_secs(1);
    loop {
        if let Ok(listener) = subscribe(storage_url).await {
            return listener;
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(RECONNECT_MAX);
    }
}

#[cfg(test)]
mod tests;
