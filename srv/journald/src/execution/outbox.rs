//! Terminal facts, not kernel event replay. Delivery leases never authorize execution.
use std::num::{NonZeroU8, NonZeroU32};
use std::time::Duration;

use chaos_snitch::tracing;
use chrono_machines::Exponential;
use outbox_machines::{
    Claimed, Confirmation, Disposition, Expired, Failure, Outbox, Refusal, State, Stored,
};

use super::*;
use crate::lease_policy::{column, epoch_millis, signed};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub version: u32,
    pub event_id: Uuid,
    #[serde(rename = "type")]
    pub kind: String,
    pub occurred_at_ms: i64,
    pub scope: String,
    pub session_id: Uuid,
    pub process_id: Option<ProcessId>,
    pub turn: Option<ExecutionTurn>,
    pub resolution_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting: Option<InvocationSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionDeliveryLease {
    pub event_id: Uuid,
    pub owner_id: String,
    /// Decimal fencing token; strictly greater on every new grant.
    pub token: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionDeliveryStatus {
    pub event_id: Uuid,
    pub session_id: Uuid,
    pub turn_id: Option<Uuid>,
    pub kind: String,
    pub created_at_ms: i64,
    pub attempts: u64,
    pub next_attempt_at_ms: i64,
    pub expires_at_ms: i64,
    pub delivered_at_ms: Option<i64>,
    pub state: ExecutionDeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDeliveryState {
    Pending,
    Claimed,
    Confirmed,
    Dead,
}

impl ExecutionDeliveryState {
    fn column(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Confirmed => "confirmed",
            Self::Dead => "dead",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Pending, Self::Claimed, Self::Confirmed, Self::Dead]
            .into_iter()
            .find(|state| state.column() == value)
    }

    fn of(state: State) -> Self {
        match state {
            State::Pending => Self::Pending,
            State::Claimed => Self::Claimed,
            State::Confirmed => Self::Confirmed,
            State::Dead => Self::Dead,
        }
    }

    fn machine(self) -> State {
        match self {
            Self::Pending => State::Pending,
            Self::Claimed => State::Claimed,
            Self::Confirmed => State::Confirmed,
            Self::Dead => State::Dead,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionDelivery {
    pub status: ExecutionDeliveryStatus,
    pub lease: ExecutionDeliveryLease,
    /// Exact immutable bytes; signatures and retries must not reserialize.
    pub body_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionDeliveryRequest {
    Configure {
        scope: String,
        destination_digest: String,
    },
    Claim {
        scope: String,
        destination_digest: String,
        owner_id: String,
        ttl_ms: u64,
    },
    Acknowledge {
        scope: String,
        destination_digest: String,
        lease: ExecutionDeliveryLease,
    },
    Retry {
        scope: String,
        destination_digest: String,
        lease: ExecutionDeliveryLease,
    },
    List {
        scope: String,
        after: Option<Uuid>,
        limit: u32,
    },
}

impl ExecutionDeliveryRequest {
    pub(crate) fn validate(&self) -> Result<(), JournalError> {
        let scope = match self {
            Self::Configure {
                scope,
                destination_digest,
            }
            | Self::Acknowledge {
                scope,
                destination_digest,
                ..
            }
            | Self::Retry {
                scope,
                destination_digest,
                ..
            } => {
                validate_digest(destination_digest)?;
                scope
            }
            Self::Claim {
                scope,
                destination_digest,
                owner_id,
                ttl_ms,
            } => {
                validate_digest(destination_digest)?;
                validate_name(owner_id)?;
                validate_ttl(*ttl_ms)?;
                scope
            }
            Self::List { scope, limit, .. } => {
                if !(1..=256).contains(limit) {
                    return Err(invalid("delivery page limit must be 1..256"));
                }
                scope
            }
        };
        validate_name(scope)
    }
}

const DELIVERY_SCOPE: u16 = 0;
const DELIVERY_RECORD: u64 = 0;
pub(super) const MAX_DELIVERY_ATTEMPTS: NonZeroU32 = match NonZeroU32::new(32) {
    Some(max) => max,
    None => panic!("delivery attempt limit is zero"),
};
const EXHAUSTED: &str = "attempts exhausted";
pub(super) const UNRESTORABLE: &str = "stored delivery state is unreachable";

type DeliveryOutbox = Outbox<1>;

#[derive(sqlx::FromRow)]
struct DeliveryRow {
    event_id: String,
    session_id: String,
    turn_id: Option<String>,
    kind: String,
    created_at_ms: i64,
    attempts: i64,
    next_attempt_at_ms: i64,
    expires_at_ms: i64,
    delivered_at_ms: Option<i64>,
    body_json: String,
    owner_id: String,
    lease_token: Option<i64>,
    last_issued: i64,
    delivery_state: String,
    failed_after_ms: Option<i64>,
    observed_at_ms: i64,
    dead_at_ms: Option<i64>,
    dead_reason: Option<String>,
}

macro_rules! delivery_select {
    ($predicate:literal, $lock:literal) => {
        concat!(
            "SELECT event_id, session_id, turn_id, kind, created_at_ms,
         attempts, next_attempt_at_ms, expires_at_ms, delivered_at_ms, body_json,
         owner_id, lease_token, last_issued, delivery_state, failed_after_ms,
         observed_at_ms, dead_at_ms, dead_reason FROM execution_outbox ",
            $predicate,
            $lock
        )
    };
}

fn millis(value: i64) -> Option<Duration> {
    u64::try_from(value).ok().map(Duration::from_millis)
}

impl DeliveryRow {
    fn state(&self) -> Option<ExecutionDeliveryState> {
        ExecutionDeliveryState::parse(&self.delivery_state)
    }

    fn outbox(&self) -> Option<DeliveryOutbox> {
        let latest = match self.lease_token {
            Some(token) => Some(Claimed::new(
                DELIVERY_RECORD,
                u64::try_from(token).ok()?,
                millis(self.expires_at_ms)?,
            )?),
            None => None,
        };
        let failed = match self.failed_after_ms {
            Some(delay) => Some(millis(delay)?),
            None => None,
        };
        let stored = Stored::new(
            DELIVERY_RECORD,
            self.state()?.machine(),
            u32::try_from(self.attempts).ok()?,
            latest,
            millis(self.next_attempt_at_ms)?,
            failed,
        )?;
        DeliveryOutbox::restored(
            MAX_DELIVERY_ATTEMPTS,
            DELIVERY_SCOPE,
            u64::try_from(self.last_issued).ok()?,
            millis(self.observed_at_ms)?,
            stored,
        )
    }

    fn record(&mut self, outbox: &DeliveryOutbox) -> Result<(), JournalError> {
        let stored = outbox
            .stored(DELIVERY_RECORD)
            .ok_or_else(|| conflict("delivery record left its outbox"))?;
        let id = &self.event_id;
        self.delivery_state = ExecutionDeliveryState::of(stored.state())
            .column()
            .to_owned();
        self.attempts = i64::from(stored.attempts());
        self.lease_token = stored
            .latest()
            .map(|claimed| signed(id, "lease_token", claimed.attempt()))
            .transpose()?;
        self.expires_at_ms = stored
            .latest()
            .map_or(Ok(0), |claimed| epoch_millis(claimed.lease_expires()))?;
        self.next_attempt_at_ms = epoch_millis(stored.available_at())?;
        self.failed_after_ms = stored.failed().map(epoch_millis).transpose()?;
        self.last_issued = signed(id, "last_issued", outbox.issued())?;
        self.observed_at_ms = epoch_millis(outbox.observed())?;
        Ok(())
    }

    fn bury(&mut self, now: i64, reason: &str) {
        self.dead_at_ms = Some(now);
        self.dead_reason = Some(reason.to_owned());
    }
}

fn refused(refusal: Refusal) -> JournalError {
    match refusal {
        Refusal::Stale | Refusal::Unknown => conflict("delivery writer was fenced"),
        other => conflict(&format!("delivery outbox refused: {other:?}")),
    }
}

fn status(row: &DeliveryRow) -> Result<ExecutionDeliveryStatus, JournalError> {
    Ok(ExecutionDeliveryStatus {
        event_id: parse_uuid(row.event_id.clone())?,
        session_id: parse_uuid(row.session_id.clone())?,
        turn_id: row.turn_id.clone().map(parse_uuid).transpose()?,
        kind: row.kind.clone(),
        created_at_ms: row.created_at_ms,
        attempts: u64::try_from(row.attempts).map_err(|_| invalid("negative delivery attempts"))?,
        next_attempt_at_ms: row.next_attempt_at_ms,
        expires_at_ms: row.expires_at_ms,
        delivered_at_ms: row.delivered_at_ms,
        state: row
            .state()
            .ok_or_else(|| invalid("unknown delivery state"))?,
        dead_reason: row.dead_reason.clone(),
    })
}

const DELIVERY_RETRY_BASE: Duration = Duration::from_secs(2);
const DELIVERY_RETRY_CAP: Duration = Duration::from_secs(3600);
const DELIVERY_RETRY: Exponential =
    match Exponential::new(DELIVERY_RETRY_BASE, 2.0, DELIVERY_RETRY_CAP, 0.5) {
        Some(shape) => shape,
        None => panic!("delivery retry shape is invalid"),
    };

fn delivery_retry_delay(attempts: i64) -> Duration {
    let attempt = u8::try_from(attempts.clamp(1, i64::from(u8::MAX)))
        .ok()
        .and_then(NonZeroU8::new)
        .unwrap_or(NonZeroU8::MAX);
    DELIVERY_RETRY.delay(attempt, chaos_ipc::backoff::draw())
}

pub(super) fn event(
    session: &ExecutionSession,
    turn: Option<ExecutionTurn>,
    resolution_id: Option<Uuid>,
    accounting: Option<InvocationSummary>,
    now: i64,
) -> Result<(ExecutionEvent, String, String), JournalError> {
    let key = match &turn {
        Some(turn) => format!("turn:{}:{}", session.session_id, turn.turn_id),
        None => format!("resolution:{}", session.session_id),
    };
    let event = ExecutionEvent {
        version: 1,
        event_id: Uuid::new_v4(),
        kind: if turn.is_some() {
            "turn.terminal"
        } else {
            "session.resolved"
        }
        .into(),
        occurred_at_ms: now,
        scope: session.scope.clone(),
        session_id: session.session_id,
        process_id: session.process_id,
        turn,
        resolution_id,
        accounting,
    };
    let body = serde_json::to_string(&event).map_err(|source| JournalError::Serialize {
        field: "execution event",
        source,
    })?;
    Ok((event, key, body))
}

// Invoked inside the execution transaction, never after releasing capacity.
macro_rules! enqueue {
    ($tx:expr, $session:expr, $turn:expr, $resolution:expr, $now:expr) => {{
        let turn = $turn;
        let accounting = if let Some(turn) = &turn {
            super::invocations::read_accounting!(
                $tx,
                $session.session_id.to_string(),
                turn.turn_id.to_string()
            )
            .summary
        } else {
            None
        };
        let (event, key, body) =
            super::outbox::event($session, turn, $resolution, accounting, $now)?;
        sqlx::query(
            "INSERT INTO execution_outbox
             (event_id, event_key, scope, session_id, turn_id, kind, created_at_ms, body_json,
              next_attempt_at_ms, observed_at_ms)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$7,$7) ON CONFLICT(event_key) DO NOTHING",
        )
        .bind(event.event_id.to_string())
        .bind(key)
        .bind(&event.scope)
        .bind(event.session_id.to_string())
        .bind(event.turn.as_ref().map(|turn| turn.turn_id.to_string()))
        .bind(&event.kind)
        .bind($now)
        .bind(body)
        .execute(&mut *$tx)
        .await?;
    }};
}
pub(super) use enqueue;

macro_rules! save {
    ($tx:expr, $row:expr) => {
        sqlx::query(
            "UPDATE execution_outbox SET delivery_state = $1, attempts = $2, lease_token = $3,
             last_issued = $4, expires_at_ms = $5, next_attempt_at_ms = $6, failed_after_ms = $7,
             observed_at_ms = $8, owner_id = $9, delivered_at_ms = $10, dead_at_ms = $11,
             dead_reason = $12 WHERE event_id = $13",
        )
        .bind(&$row.delivery_state).bind($row.attempts).bind($row.lease_token)
        .bind($row.last_issued).bind($row.expires_at_ms).bind($row.next_attempt_at_ms)
        .bind($row.failed_after_ms).bind($row.observed_at_ms).bind(&$row.owner_id)
        .bind($row.delivered_at_ms).bind($row.dead_at_ms).bind(&$row.dead_reason)
        .bind(&$row.event_id).execute(&mut *$tx).await?
    };
}

macro_rules! quarantine {
    ($tx:expr, $row:expr, $now:expr) => {{
        tracing::warn!(
            event_id = %$row.event_id,
            state = %$row.delivery_state,
            "{UNRESTORABLE}; delivery marked dead"
        );
        sqlx::query(
            "UPDATE execution_outbox SET delivery_state = 'dead', dead_at_ms = $1, dead_reason = $2
             WHERE event_id = $3 AND delivery_state IN ('pending', 'claimed')",
        )
        .bind($now).bind(UNRESTORABLE).bind(&$row.event_id).execute(&mut *$tx).await?;
    }};
}

macro_rules! impl_outbox {
    ($store:ty, $begin:expr, $lock:literal, $clock:literal) => {
        impl $store {
            pub(crate) async fn execute_delivery(
                &self,
                request: ExecutionDeliveryRequest,
            ) -> Result<ExecutionResponse, JournalError> {
                request.validate()?;
                if let ExecutionDeliveryRequest::List {
                    scope,
                    after,
                    limit,
                } = &request
                {
                    let rows = sqlx::query_as::<_, DeliveryRow>(delivery_select!(
                        "WHERE scope = $1 AND event_id > $2 ORDER BY event_id LIMIT $3",
                        ""
                    ))
                    .bind(scope)
                    .bind(after.map(|id| id.to_string()).unwrap_or_default())
                    .bind(i64::from(*limit))
                    .fetch_all(self.pool())
                    .await?;
                    return Ok(ExecutionResponse::Deliveries(
                        rows.iter().map(status).collect::<Result<_, _>>()?,
                    ));
                }
                let (scope, digest) = match &request {
                    ExecutionDeliveryRequest::Configure {
                        scope,
                        destination_digest,
                    }
                    | ExecutionDeliveryRequest::Claim {
                        scope,
                        destination_digest,
                        ..
                    }
                    | ExecutionDeliveryRequest::Acknowledge {
                        scope,
                        destination_digest,
                        ..
                    }
                    | ExecutionDeliveryRequest::Retry {
                        scope,
                        destination_digest,
                        ..
                    } => (scope, destination_digest),
                    _ => unreachable!(),
                };
                let mut tx = self.pool().begin_with($begin).await?;
                // Serializes all workers and route bootstrap within a namespace.
                sqlx::query_scalar::<_, String>(concat!(
                    "SELECT scope FROM execution_scopes WHERE scope = $1",
                    $lock
                ))
                .bind(scope)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| conflict("delivery scope does not exist"))?;
                if matches!(request, ExecutionDeliveryRequest::Configure { .. }) {
                    sqlx::query(
                        "INSERT INTO execution_delivery_targets (scope, destination_digest)
                        VALUES ($1,$2) ON CONFLICT(scope) DO NOTHING",
                    )
                    .bind(scope)
                    .bind(digest)
                    .execute(&mut *tx)
                    .await?;
                }
                let configured = sqlx::query_scalar::<_, String>(
                    "SELECT destination_digest FROM execution_delivery_targets WHERE scope = $1",
                )
                .bind(scope)
                .fetch_optional(&mut *tx)
                .await?;
                if configured.as_deref() != Some(digest.as_str()) {
                    return Err(conflict("delivery destination or signing identity changed"));
                }
                let now: i64 = sqlx::query_scalar($clock).fetch_one(&mut *tx).await?;
                let response = match &request {
                    ExecutionDeliveryRequest::Configure { .. } => {
                        ExecutionResponse::DeliveryRecorded
                    }
                    ExecutionDeliveryRequest::Claim {
                        owner_id, ttl_ms, ..
                    } => {
                        let clock = super::storage::clock(now)?;
                        loop {
                            let Some(mut row) = sqlx::query_as::<_, DeliveryRow>(delivery_select!(
                                "WHERE scope = $1 AND (
                                    (delivery_state = 'pending' AND next_attempt_at_ms <= $2)
                                    OR (delivery_state = 'claimed' AND expires_at_ms <= $2))
                                 ORDER BY next_attempt_at_ms, event_id LIMIT 1",
                                $lock
                            ))
                            .bind(scope)
                            .bind(now)
                            .fetch_optional(&mut *tx)
                            .await?
                            else {
                                break ExecutionResponse::Delivery(None);
                            };
                            let Some(mut outbox) = row.outbox() else {
                                quarantine!(tx, row, now);
                                continue;
                            };
                            let at = clock.max(outbox.observed());
                            if row.state() == Some(ExecutionDeliveryState::Claimed) {
                                outbox
                                    .expire_claims(at, 1, &mut [Expired::default()])
                                    .map_err(refused)?;
                            }
                            let claimed = outbox
                                .claim(at, Duration::from_millis(*ttl_ms))
                                .map_err(refused)?;
                            row.record(&outbox)?;
                            let Some(claimed) = claimed else {
                                if row.state() != Some(ExecutionDeliveryState::Dead) {
                                    return Err(conflict("due delivery could not be claimed"));
                                }
                                row.bury(now, EXHAUSTED);
                                save!(tx, row);
                                continue;
                            };
                            row.owner_id = owner_id.clone();
                            save!(tx, row);
                            let status = status(&row)?;
                            break ExecutionResponse::Delivery(Some(ExecutionDelivery {
                                lease: ExecutionDeliveryLease {
                                    event_id: status.event_id,
                                    owner_id: owner_id.clone(),
                                    token: claimed.attempt().to_string(),
                                    expires_at_ms: row.expires_at_ms,
                                },
                                status,
                                body_json: row.body_json,
                            }));
                        }
                    }
                    ExecutionDeliveryRequest::Acknowledge { lease, .. }
                    | ExecutionDeliveryRequest::Retry { lease, .. } => {
                        let mut row = sqlx::query_as::<_, DeliveryRow>(delivery_select!(
                            "WHERE scope = $1 AND event_id = $2",
                            $lock
                        ))
                        .bind(scope)
                        .bind(lease.event_id.to_string())
                        .fetch_optional(&mut *tx)
                        .await?
                        .ok_or(JournalError::ExecutionNotFound)?;
                        let fenced = || conflict("delivery writer was fenced");
                        if row.owner_id != lease.owner_id {
                            return Err(fenced());
                        }
                        let attempt: u64 = lease.token.parse().map_err(|_| fenced())?;
                        let Some(mut outbox) = row.outbox() else {
                            quarantine!(tx, row, now);
                            tx.commit().await?;
                            return Err(conflict(UNRESTORABLE));
                        };
                        let at = super::storage::clock(now)?.max(outbox.observed());
                        if matches!(request, ExecutionDeliveryRequest::Acknowledge { .. }) {
                            if outbox.confirm(DELIVERY_RECORD, attempt).map_err(refused)?
                                == Confirmation::Confirmed
                            {
                                row.record(&outbox)?;
                                row.delivered_at_ms = Some(now);
                                save!(tx, row);
                            }
                        } else {
                            let retry_after = match (row.state(), row.failed_after_ms) {
                                (Some(ExecutionDeliveryState::Claimed), _) | (_, None) => {
                                    delivery_retry_delay(row.attempts)
                                }
                                (_, Some(delay)) => Duration::from_millis(column(
                                    &row.event_id,
                                    "failed_after_ms",
                                    delay,
                                )?),
                            };
                            let failure = outbox
                                .fail(DELIVERY_RECORD, attempt, at, retry_after)
                                .map_err(refused)?;
                            if let Failure::Failed(disposition) = failure {
                                row.record(&outbox)?;
                                if disposition == Disposition::Dead {
                                    row.bury(now, EXHAUSTED);
                                }
                                save!(tx, row);
                            }
                        }
                        ExecutionResponse::DeliveryRecorded
                    }
                    _ => unreachable!(),
                };
                tx.commit().await?;
                Ok(response)
            }
        }
    };
}
impl_outbox!(
    crate::SqliteJournalStore,
    "BEGIN IMMEDIATE",
    "",
    "SELECT CAST(strftime('%s', 'now') AS INTEGER) * 1000 +
     CAST(substr(strftime('%f', 'now'), 4, 3) AS INTEGER)"
);
impl_outbox!(
    crate::PostgresJournalStore,
    "BEGIN",
    " FOR UPDATE",
    "SELECT CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)"
);
