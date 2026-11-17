//! Bounded, lease-fenced observations; never execution authority.
use core::num::NonZeroU64;

use journal_machines::{Coverage, ReplayWindow};

use super::*;

pub(super) const MAX_EVENTS: usize = 256;
const MAX_EVENT_BYTES: usize = 64 * 1024;
pub(super) const MAX_REPLAY_BYTES: NonZeroU64 =
    NonZeroU64::new(1024 * 1024).expect("nonzero budget");

type Window = ReplayWindow<MAX_EVENTS>;

fn restore(current: i64, retained: &[(i64, i64)]) -> Result<Window, JournalError> {
    let sizes = retained
        .iter()
        .map(|&(_, size)| u64::try_from(size))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| conflict("stored replay size is negative"))?;
    let next = u64::try_from(current)
        .ok()
        .and_then(|current| current.checked_add(1))
        .ok_or_else(|| conflict("stored replay sequence is out of range"))?;
    Window::restored(Some(MAX_REPLAY_BYTES), 1, next, &sizes)
        .filter(|window| {
            retained
                .first()
                .is_none_or(|&(sequence, _)| u64::try_from(sequence) == Ok(window.floor()))
        })
        .ok_or_else(|| conflict("stored replay window is inconsistent"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionReplayRequest {
    Append {
        lease: ExecutionLease,
        /// Compare-and-append, not an instruction to replay execution.
        after: u64,
        event: serde_json::Value,
    },
    Read {
        session_id: Uuid,
        after: u64,
    },
}

impl ExecutionReplayRequest {
    pub(crate) fn validate(&self) -> Result<(), JournalError> {
        let after = match self {
            Self::Append { after, event, .. } => {
                if serde_json::to_vec(event)
                    .map_err(|source| JournalError::Serialize {
                        field: "replay event",
                        source,
                    })?
                    .len()
                    > MAX_EVENT_BYTES
                {
                    return Err(invalid("replay event exceeds bound"));
                }
                *after
            }
            Self::Read { after, .. } => *after,
        };
        if after > i64::MAX as u64 {
            return Err(invalid("replay cursor exceeds bound"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReplayEvent {
    pub sequence: u64,
    pub event: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReplayBatch {
    pub events: Vec<ExecutionReplayEvent>,
    pub gap: bool,
    pub next_cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExecutionReplayResponse {
    Appended { sequence: u64 },
    Batch(ExecutionReplayBatch),
}

macro_rules! impl_replay {
    ($name:ident, $pool:ty, $begin:literal, $lock:literal, $clock:literal) => {
        pub(crate) async fn $name(
            pool: &$pool,
            request: ExecutionReplayRequest,
        ) -> Result<ExecutionReplayResponse, JournalError> {
            use super::storage::{SessionRow, check_lease, session_select_sql};
            request.validate()?;
            let session_id = match &request {
                ExecutionReplayRequest::Append { lease, .. } => lease.session_id,
                ExecutionReplayRequest::Read { session_id, .. } => *session_id,
            }.to_string();
            let mut tx = pool.begin_with($begin).await?;
            // Share the execution row lock with every ownership transition.
            let row = sqlx::query_as::<_, SessionRow>(session_select_sql!(
                "WHERE session_id = $1", $lock
            )).bind(&session_id).fetch_optional(&mut *tx).await?
                .ok_or(JournalError::ExecutionNotFound)?;
            let current: i64 = sqlx::query_scalar(
                "SELECT sequence FROM execution_replay WHERE session_id = $1"
            ).bind(&session_id).fetch_optional(&mut *tx).await?.unwrap_or(0);
            let response = match request {
                ExecutionReplayRequest::Append { lease, after, event } => {
                    let now: i64 = sqlx::query_scalar($clock).fetch_one(&mut *tx).await?;
                    check_lease(&row, &lease, now)?;
                    if row.3 == "closed" {
                        return Err(conflict("closed sessions cannot append observations"));
                    }
                    let next = (after as i64).checked_add(1)
                        .ok_or_else(|| invalid("replay sequence exhausted"))?;
                    let json = serde_json::to_string(&event)
                        .map_err(|source| JournalError::Serialize { field: "replay event", source })?;
                    if current != after as i64 {
                        // A lost append response may reconcile only the exact
                        // already-committed observation, with current ownership.
                        let previous: Option<String> = sqlx::query_scalar(
                            "SELECT event_json FROM execution_replay_events WHERE session_id = $1 AND sequence = $2"
                        ).bind(&session_id).bind(next).fetch_optional(&mut *tx).await?;
                        if current != next || previous.as_deref() != Some(json.as_str()) {
                            return Err(conflict("replay cursor changed; reconcile observations"));
                        }
                    } else {
                        let retained: Vec<(i64, i64)> = sqlx::query_as(
                            "SELECT sequence, event_bytes FROM execution_replay_events
                            WHERE session_id = $1 ORDER BY sequence"
                        ).bind(&session_id).fetch_all(&mut *tx).await?;
                        let mut window = restore(current, &retained)?;
                        let floor = window.floor();
                        let appended = window.append(json.len() as u64)
                            .map_err(|_| invalid("replay sequence exhausted"))?;
                        sqlx::query("INSERT INTO execution_replay_events (session_id, sequence, event_json, event_bytes)
                            VALUES ($1,$2,$3,$4)")
                            .bind(&session_id).bind(next).bind(&json).bind(json.len() as i64)
                            .execute(&mut *tx).await?;
                        sqlx::query("INSERT INTO execution_replay (session_id, sequence) VALUES ($1,$2)
                            ON CONFLICT(session_id) DO UPDATE SET sequence = excluded.sequence")
                            .bind(&session_id).bind(next).execute(&mut *tx).await?;
                        if appended.floor() > floor {
                            sqlx::query("DELETE FROM execution_replay_events
                                WHERE session_id = $1 AND sequence >= $2 AND sequence < $3")
                                .bind(&session_id).bind(floor as i64)
                                .bind(appended.floor() as i64)
                                .execute(&mut *tx).await?;
                        }
                    }
                    ExecutionReplayResponse::Appended { sequence: next as u64 }
                }
                ExecutionReplayRequest::Read { after, .. } => {
                    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
                        "SELECT sequence, event_json, event_bytes FROM execution_replay_events
                        WHERE session_id = $1 ORDER BY sequence"
                    ).bind(&session_id).fetch_all(&mut *tx).await?;
                    let retained: Vec<(i64, i64)> =
                        rows.iter().map(|row| (row.0, row.2)).collect();
                    let gap = match restore(current, &retained)?.coverage(after + 1) {
                        Coverage::Ahead => {
                            return Err(invalid("event cursor is ahead of this session"));
                        }
                        Coverage::Behind { .. } => true,
                        Coverage::Covered => false,
                    };
                    let events = rows.into_iter().filter(|row| row.0 as u64 > after)
                        .map(|(sequence, json, _)| Ok(ExecutionReplayEvent {
                            sequence: sequence as u64, event: decode(&json)?,
                        })).collect::<Result<_, JournalError>>()?;
                    ExecutionReplayResponse::Batch(ExecutionReplayBatch {
                        events, gap, next_cursor: current as u64,
                    })
                }
            };
            tx.commit().await?;
            Ok(response)
        }
    };
}

impl_replay!(
    execute_replay_sqlite,
    sqlx::SqlitePool,
    "BEGIN IMMEDIATE",
    "",
    "SELECT CAST(strftime('%s', 'now') AS INTEGER) * 1000 + CAST(substr(strftime('%f', 'now'), 4, 3) AS INTEGER)"
);
impl_replay!(
    execute_replay_postgres,
    sqlx::PgPool,
    "BEGIN",
    " FOR UPDATE",
    "SELECT CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)"
);
