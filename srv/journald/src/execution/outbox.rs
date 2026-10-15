//! Terminal facts, not kernel event replay. Delivery leases never authorize execution.
use super::*;

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
    pub token: Uuid,
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

type DeliveryRow = (
    String,
    String,
    Option<String>,
    String,
    i64,
    i64,
    i64,
    i64,
    Option<i64>,
    String,
    String,
    Option<String>,
);
macro_rules! delivery_select {
    ($predicate:literal, $lock:literal) => {
        concat!(
            "SELECT event_id, session_id, turn_id, kind, created_at_ms,
         attempts, next_attempt_at_ms, expires_at_ms, delivered_at_ms, body_json,
         owner_id, lease_token FROM execution_outbox ",
            $predicate,
            $lock
        )
    };
}

fn status(row: &DeliveryRow) -> Result<ExecutionDeliveryStatus, JournalError> {
    Ok(ExecutionDeliveryStatus {
        event_id: parse_uuid(row.0.clone())?,
        session_id: parse_uuid(row.1.clone())?,
        turn_id: row.2.clone().map(parse_uuid).transpose()?,
        kind: row.3.clone(),
        created_at_ms: row.4,
        attempts: u64::try_from(row.5).map_err(|_| invalid("negative delivery attempts"))?,
        next_attempt_at_ms: row.6,
        expires_at_ms: row.7,
        delivered_at_ms: row.8,
    })
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
            super::invocations::read_accounting!($tx, $session.session_id.to_string(), turn.turn_id.to_string()).summary
        } else { None };
        let (event, key, body) = super::outbox::event($session, turn, $resolution, accounting, $now)?;
        sqlx::query(
            "INSERT INTO execution_outbox
             (event_id, event_key, scope, session_id, turn_id, kind, created_at_ms, body_json, next_attempt_at_ms)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$7) ON CONFLICT(event_key) DO NOTHING",
        )
        .bind(event.event_id.to_string()).bind(key).bind(&event.scope)
        .bind(event.session_id.to_string()).bind(event.turn.as_ref().map(|turn| turn.turn_id.to_string()))
        .bind(&event.kind).bind($now).bind(body).execute(&mut *$tx).await?;
    }};
}
pub(super) use enqueue;

macro_rules! impl_outbox {
    ($store:ty, $begin:expr, $lock:literal, $clock:literal) => {
        impl $store {
            pub(crate) async fn execute_delivery(&self, request: ExecutionDeliveryRequest) -> Result<ExecutionResponse, JournalError> {
                request.validate()?;
                if let ExecutionDeliveryRequest::List { scope, after, limit } = &request {
                    let rows = sqlx::query_as::<_, DeliveryRow>(delivery_select!(
                        "WHERE scope = $1 AND event_id > $2 ORDER BY event_id LIMIT $3", ""
                    )).bind(scope).bind(after.map(|id| id.to_string()).unwrap_or_default())
                        .bind(i64::from(*limit)).fetch_all(self.pool()).await?;
                    return Ok(ExecutionResponse::Deliveries(rows.iter().map(status).collect::<Result<_,_>>()?));
                }
                let (scope, digest) = match &request {
                    ExecutionDeliveryRequest::Configure { scope, destination_digest }
                    | ExecutionDeliveryRequest::Claim { scope, destination_digest, .. }
                    | ExecutionDeliveryRequest::Acknowledge { scope, destination_digest, .. }
                    | ExecutionDeliveryRequest::Retry { scope, destination_digest, .. } => (scope, destination_digest),
                    _ => unreachable!(),
                };
                let mut tx = self.pool().begin_with($begin).await?;
                // Serializes all workers and route bootstrap within a namespace.
                sqlx::query_scalar::<_, String>(concat!(
                    "SELECT scope FROM execution_scopes WHERE scope = $1", $lock
                )).bind(scope).fetch_optional(&mut *tx).await?
                    .ok_or_else(|| conflict("delivery scope does not exist"))?;
                if matches!(request, ExecutionDeliveryRequest::Configure { .. }) {
                    sqlx::query("INSERT INTO execution_delivery_targets (scope, destination_digest)
                        VALUES ($1,$2) ON CONFLICT(scope) DO NOTHING")
                        .bind(scope).bind(digest).execute(&mut *tx).await?;
                }
                let configured = sqlx::query_scalar::<_, String>(
                    "SELECT destination_digest FROM execution_delivery_targets WHERE scope = $1"
                ).bind(scope).fetch_optional(&mut *tx).await?;
                if configured.as_deref() != Some(digest.as_str()) {
                    return Err(conflict("delivery destination or signing identity changed"));
                }
                let now: i64 = sqlx::query_scalar($clock).fetch_one(&mut *tx).await?;
                let response = match &request {
                    ExecutionDeliveryRequest::Configure { .. } => ExecutionResponse::DeliveryRecorded,
                    ExecutionDeliveryRequest::Claim { owner_id, ttl_ms, .. } => {
                        let row = sqlx::query_as::<_, DeliveryRow>(delivery_select!(
                            "WHERE scope = $1 AND delivered_at_ms IS NULL
                             AND next_attempt_at_ms <= $2 AND expires_at_ms <= $2
                             ORDER BY next_attempt_at_ms, event_id LIMIT 1", $lock
                        )).bind(scope).bind(now).fetch_optional(&mut *tx).await?;
                        if let Some(mut row) = row {
                            let token = Uuid::new_v4();
                            row.5 = row.5.saturating_add(1);
                            row.7 = now + *ttl_ms as i64;
                            sqlx::query("UPDATE execution_outbox SET owner_id = $1, lease_token = $2,
                                expires_at_ms = $3, attempts = $4 WHERE event_id = $5")
                                .bind(owner_id).bind(token.to_string()).bind(row.7).bind(row.5)
                                .bind(&row.0).execute(&mut *tx).await?;
                            let status = status(&row)?;
                            ExecutionResponse::Delivery(Some(ExecutionDelivery {
                                lease: ExecutionDeliveryLease { event_id: status.event_id, owner_id: owner_id.clone(), token, expires_at_ms: row.7 },
                                status, body_json: row.9,
                            }))
                        } else {
                            ExecutionResponse::Delivery(None)
                        }
                    }
                    ExecutionDeliveryRequest::Acknowledge { lease, .. }
                    | ExecutionDeliveryRequest::Retry { lease, .. } => {
                        let row = sqlx::query_as::<_, DeliveryRow>(delivery_select!(
                            "WHERE scope = $1 AND event_id = $2", $lock
                        )).bind(scope).bind(lease.event_id.to_string())
                            .fetch_optional(&mut *tx).await?.ok_or(JournalError::ExecutionNotFound)?;
                        if row.10 != lease.owner_id || row.11.as_deref() != Some(lease.token.to_string().as_str()) {
                            return Err(conflict("delivery writer was fenced"));
                        }
                        if row.8.is_some() && matches!(request, ExecutionDeliveryRequest::Acknowledge { .. }) {
                            // Identical lost-ack retries are read-only, even after expiry.
                        } else {
                            if row.8.is_some() || row.7 <= now {
                                return Err(conflict("delivery lease expired or event already acknowledged"));
                            }
                            if matches!(request, ExecutionDeliveryRequest::Acknowledge { .. }) {
                                sqlx::query("UPDATE execution_outbox SET delivered_at_ms = $1, expires_at_ms = 0
                                    WHERE event_id = $2").bind(now).bind(&row.0).execute(&mut *tx).await?;
                            } else {
                                let backoff_ms = (1_i64 << row.5.clamp(1,12)).min(3600) * 1000;
                                sqlx::query("UPDATE execution_outbox SET next_attempt_at_ms = $1,
                                    expires_at_ms = 0, lease_token = NULL, owner_id = '' WHERE event_id = $2")
                                    .bind(now + backoff_ms).bind(&row.0).execute(&mut *tx).await?;
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
