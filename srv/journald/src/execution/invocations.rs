//! Observed logical requests, never reconstructed deltas or provider invoices.
use super::*;

pub const MAX_EXECUTION_INVOCATIONS: i64 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationPurpose {
    Sampling,
    Compaction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationStart {
    pub invocation_id: Uuid,
    pub turn_id: Uuid,
    pub process_id: ProcessId,
    pub provider_id: String,
    pub requested_model: String,
    pub requested_service_tier: Option<chaos_ipc::config_types::ServiceTier>,
    pub purpose: InvocationPurpose,
}

/// Provider-adapter-normalized counters; no estimates, costs or dispatch count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationTokenUsage {
    pub input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

impl From<&chaos_ipc::protocol::TokenUsage> for InvocationTokenUsage {
    fn from(usage: &chaos_ipc::protocol::TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_output_tokens: usage.reasoning_output_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

impl InvocationTokenUsage {
    fn validate(&self) -> Result<(), JournalError> {
        if self.input_tokens < 0
            || self.cache_creation_input_tokens < 0
            || self.cached_input_tokens < 0
            || self.output_tokens < 0
            || self.reasoning_output_tokens < 0
            || self.total_tokens < 0
            || self
                .cache_creation_input_tokens
                .checked_add(self.cached_input_tokens)
                .is_none_or(|cached| cached > self.input_tokens)
            || self.reasoning_output_tokens > self.output_tokens
        {
            return Err(invalid("invalid provider usage counters"));
        }
        Ok(())
    }

    fn checked_add(&self, usage: &Self) -> Option<Self> {
        Some(Self {
            input_tokens: self.input_tokens.checked_add(usage.input_tokens)?,
            cache_creation_input_tokens: self
                .cache_creation_input_tokens
                .checked_add(usage.cache_creation_input_tokens)?,
            cached_input_tokens: self
                .cached_input_tokens
                .checked_add(usage.cached_input_tokens)?,
            output_tokens: self.output_tokens.checked_add(usage.output_tokens)?,
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .checked_add(usage.reasoning_output_tokens)?,
            total_tokens: self.total_tokens.checked_add(usage.total_tokens)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationReceipt {
    /// Observed a completed response, not proof of a billable transport attempt.
    pub completed: bool,
    pub response_id: Option<String>,
    pub server_model: Option<String>,
    pub usage: Option<InvocationTokenUsage>,
}

impl InvocationReceipt {
    pub fn unknown() -> Self {
        Self {
            completed: false,
            response_id: None,
            server_model: None,
            usage: None,
        }
    }
    fn validate(&self) -> Result<(), JournalError> {
        for value in [&self.response_id, &self.server_model]
            .into_iter()
            .flatten()
        {
            if value.is_empty() || value.len() > 512 {
                return Err(invalid("provider receipt metadata exceeds bounds"));
            }
        }
        if !self.completed
            && (self.response_id.is_some() || self.server_model.is_some() || self.usage.is_some())
        {
            return Err(invalid(
                "unknown invocation cannot claim a provider receipt",
            ));
        }
        if let Some(usage) = &self.usage {
            usage.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationState {
    Started,
    Completed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionInvocation {
    pub request: InvocationStart,
    pub state: InvocationState,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub receipt: Option<InvocationReceipt>,
    pub duplicate_receipt_of: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationSummary {
    pub billing_complete: bool,
    pub coverage: String,
    pub recorded_invocations: u64,
    pub reported_invocations: u64,
    pub completed_without_usage: u64,
    pub unknown_invocations: u64,
    pub pending_invocations: u64,
    pub duplicate_receipts: u64,
    pub known_usage: Option<InvocationTokenUsage>,
    pub totals_overflowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAccounting {
    /// False for pre-metering/legacy turns. An absent ledger is not zero usage.
    pub enabled: bool,
    pub summary: Option<InvocationSummary>,
    pub invocations: Vec<ExecutionInvocation>,
}

pub(super) fn accounting(
    enabled: bool,
    invocations: Vec<ExecutionInvocation>,
) -> ExecutionAccounting {
    let summary = enabled.then(|| {
        let mut summary = InvocationSummary {
            billing_complete: false,
            coverage: "direct_logical_sampling_and_compaction".into(),
            recorded_invocations: invocations.len() as u64,
            reported_invocations: 0,
            completed_without_usage: 0,
            unknown_invocations: 0,
            pending_invocations: 0,
            duplicate_receipts: 0,
            known_usage: Some(InvocationTokenUsage::default()),
            totals_overflowed: false,
        };
        for invocation in &invocations {
            match invocation.state {
                InvocationState::Started => summary.pending_invocations += 1,
                InvocationState::Unknown => summary.unknown_invocations += 1,
                InvocationState::Completed => {
                    if invocation.duplicate_receipt_of.is_some() {
                        summary.duplicate_receipts += 1;
                    } else if let Some(usage) = invocation
                        .receipt
                        .as_ref()
                        .and_then(|receipt| receipt.usage.as_ref())
                    {
                        summary.reported_invocations += 1;
                        summary.known_usage = summary
                            .known_usage
                            .as_ref()
                            .and_then(|total| total.checked_add(usage));
                        summary.totals_overflowed |= summary.known_usage.is_none();
                    } else {
                        summary.completed_without_usage += 1;
                    }
                }
            }
        }
        if summary.reported_invocations == 0 {
            summary.known_usage = None;
        }
        summary
    });
    ExecutionAccounting {
        enabled,
        summary,
        invocations,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionInvocationRequest {
    Begin {
        lease: ExecutionLease,
        request: InvocationStart,
    },
    Complete {
        lease: ExecutionLease,
        invocation_id: Uuid,
        receipt: InvocationReceipt,
    },
    Read {
        session_id: Uuid,
        turn_id: Uuid,
    },
}

impl ExecutionInvocationRequest {
    pub(crate) fn validate(&self) -> Result<(), JournalError> {
        match self {
            Self::Begin { request, .. } => {
                validate_name(&request.provider_id)?;
                validate_name(&request.requested_model)?;
            }
            Self::Complete { receipt, .. } => receipt.validate()?,
            Self::Read { .. } => {}
        }
        Ok(())
    }
}

pub(super) type InvocationRow = (
    String,
    String,
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
);
pub(super) const SELECT_INVOCATIONS: &str =
    "SELECT request_json, state, started_at_ms, finished_at_ms, receipt_json, duplicate_receipt_of FROM execution_invocations
     WHERE session_id = $1 AND turn_id = $2 ORDER BY started_at_ms, invocation_id";

pub(super) fn invocation(row: InvocationRow) -> Result<ExecutionInvocation, JournalError> {
    Ok(ExecutionInvocation {
        request: decode(&row.0)?,
        state: decode_state(row.1)?,
        started_at_ms: row.2,
        finished_at_ms: row.3,
        receipt: row.4.as_deref().map(decode).transpose()?,
        duplicate_receipt_of: row.5.map(parse_uuid).transpose()?,
    })
}

macro_rules! read_accounting {
    ($tx:expr, $session_id:expr, $turn_id:expr) => {{
        let enabled: i32 = sqlx::query_scalar(
            "SELECT accounting_enabled FROM execution_turns
            WHERE session_id = $1 AND turn_id = $2",
        )
        .bind($session_id)
        .bind($turn_id)
        .fetch_optional(&mut *$tx)
        .await?
        .ok_or(JournalError::ExecutionNotFound)?;
        let rows = sqlx::query_as::<_, super::invocations::InvocationRow>(
            super::invocations::SELECT_INVOCATIONS,
        )
        .bind($session_id)
        .bind($turn_id)
        .fetch_all(&mut *$tx)
        .await?;
        super::invocations::accounting(
            enabled != 0,
            rows.into_iter()
                .map(super::invocations::invocation)
                .collect::<Result<_, _>>()?,
        )
    }};
}
pub(super) use read_accounting;

macro_rules! fence_invocations {
    ($tx:expr, $session_id:expr, $turn_id:expr, $now:expr) => {
        sqlx::query(
            "UPDATE execution_invocations SET state = 'unknown', finished_at_ms = $1
            WHERE session_id = $2 AND turn_id = $3 AND state = 'started'",
        )
        .bind($now)
        .bind($session_id)
        .bind($turn_id)
        .execute(&mut *$tx)
        .await?;
    };
}
pub(super) use fence_invocations;

macro_rules! impl_invocations {
    ($store:ty, $begin:expr, $lock:literal, $clock:literal) => {
        impl $store {
            pub(crate) async fn execute_invocation(&self, request: ExecutionInvocationRequest) -> Result<ExecutionResponse, JournalError> {
                use super::storage::{SessionRow, check_lease, session};
                request.validate()?;
                let session_id = match &request {
                    ExecutionInvocationRequest::Begin { lease, .. } | ExecutionInvocationRequest::Complete { lease, .. } => lease.session_id,
                    ExecutionInvocationRequest::Read { session_id, .. } => *session_id,
                }.to_string();
                let mut tx = self.pool().begin_with($begin).await?;
                let row = sqlx::query_as::<_, SessionRow>(concat!(
                    "SELECT session_id, scope, process_id, state, active_turn_id,
                     owner_id, lease_token, expires_at_ms,
                     (SELECT COUNT(*) FROM execution_turns t WHERE t.session_id = execution_sessions.session_id),
                     recovery_epoch FROM execution_sessions WHERE session_id = $1", $lock
                )).bind(&session_id).fetch_optional(&mut *tx).await?.ok_or(JournalError::ExecutionNotFound)?;
                let now: i64 = sqlx::query_scalar($clock).fetch_one(&mut *tx).await?;
                let response = match request {
                    ExecutionInvocationRequest::Read { turn_id, .. } => {
                        ExecutionResponse::Accounting(read_accounting!(tx, &session_id, turn_id.to_string()))
                    }
                    ExecutionInvocationRequest::Begin { lease, request } => {
                        check_lease(&row, &lease, now)?;
                        let previous = sqlx::query_as::<_, InvocationRow>(
                            "SELECT request_json, state, started_at_ms, finished_at_ms, receipt_json, duplicate_receipt_of
                             FROM execution_invocations WHERE invocation_id = $1 AND session_id = $2"
                        ).bind(request.invocation_id.to_string()).bind(&session_id)
                            .fetch_optional(&mut *tx).await?.map(invocation).transpose()?;
                        if let Some(previous) = previous {
                            if previous.request != request { return Err(conflict("invocation identity is immutable")); }
                            ExecutionResponse::InvocationBegan { invocation: previous, newly_recorded: false }
                        } else {
                            let current = session(&row)?;
                            if current.state != ExecutionSessionState::Active
                                || current.active_turn_id != Some(request.turn_id)
                                || current.process_id != Some(request.process_id)
                            { return Err(conflict("invocation is not bound to the active process/turn")); }
                            let state = sqlx::query_as::<_, (String, i32)>(
                                "SELECT state, accounting_enabled FROM execution_turns WHERE session_id = $1 AND turn_id = $2"
                            ).bind(&session_id).bind(request.turn_id.to_string()).fetch_one(&mut *tx).await?;
                            if state.0 != "running" || state.1 == 0 { return Err(conflict("invocation requires dispatched metered execution")); }
                            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_invocations WHERE session_id = $1 AND turn_id = $2")
                                .bind(&session_id).bind(request.turn_id.to_string()).fetch_one(&mut *tx).await?;
                            if count >= MAX_EXECUTION_INVOCATIONS { return Err(conflict("invocation limit reached before provider dispatch")); }
                            let json = serde_json::to_string(&request).map_err(|source| JournalError::Serialize { field: "invocation start", source })?;
                            sqlx::query("INSERT INTO execution_invocations
                                (invocation_id, session_id, turn_id, request_json, started_at_ms, state)
                                VALUES ($1,$2,$3,$4,$5,'started')")
                                .bind(request.invocation_id.to_string()).bind(&session_id).bind(request.turn_id.to_string())
                                .bind(json).bind(now).execute(&mut *tx).await?;
                            ExecutionResponse::InvocationBegan { invocation: ExecutionInvocation {
                                request, state: InvocationState::Started, started_at_ms: now, finished_at_ms: None, receipt: None, duplicate_receipt_of: None,
                            }, newly_recorded: true }
                        }
                    }
                    ExecutionInvocationRequest::Complete { lease, invocation_id, receipt } => {
                        let previous = sqlx::query_as::<_, InvocationRow>(
                            "SELECT request_json, state, started_at_ms, finished_at_ms, receipt_json, duplicate_receipt_of
                             FROM execution_invocations WHERE invocation_id = $1 AND session_id = $2"
                        ).bind(invocation_id.to_string()).bind(&session_id).fetch_optional(&mut *tx).await?
                            .ok_or(JournalError::ExecutionNotFound)?;
                        let mut previous = invocation(previous)?;
                        if previous.state != InvocationState::Started {
                            let identical = previous.receipt.as_ref() == Some(&receipt)
                                || previous.state == InvocationState::Unknown && previous.receipt.is_none() && receipt == InvocationReceipt::unknown();
                            if !identical { return Err(conflict("invocation receipt is immutable")); }
                        } else {
                            check_lease(&row, &lease, now)?;
                            let current = session(&row)?;
                            if current.active_turn_id != Some(previous.request.turn_id)
                                || current.state != ExecutionSessionState::Active
                            { return Err(conflict("invocation writer no longer owns active execution")); }
                            let state = if receipt.completed { "completed" } else { "unknown" };
                            let json = serde_json::to_string(&receipt).map_err(|source| JournalError::Serialize { field: "invocation receipt", source })?;
                            if let Some(response_id) = &receipt.response_id {
                                // Domain-separated tuple, never an ambiguous joined string.
                                let key = serde_json::to_string(&("provider-receipt/v1", &current.scope, &previous.request.provider_id, response_id))
                                    .map_err(|source| JournalError::Serialize { field: "provider receipt identity", source })?;
                                sqlx::query("INSERT INTO execution_provider_receipts (receipt_key, invocation_id)
                                    VALUES ($1,$2) ON CONFLICT(receipt_key) DO NOTHING")
                                    .bind(&key).bind(invocation_id.to_string()).execute(&mut *tx).await?;
                                let owner: String = sqlx::query_scalar("SELECT invocation_id FROM execution_provider_receipts WHERE receipt_key = $1")
                                    .bind(key).fetch_one(&mut *tx).await?;
                                let owner = parse_uuid(owner)?;
                                previous.duplicate_receipt_of = (owner != invocation_id).then_some(owner);
                            }
                            sqlx::query("UPDATE execution_invocations SET state = $1, receipt_json = $2, finished_at_ms = $3,
                                duplicate_receipt_of = $4 WHERE invocation_id = $5")
                                .bind(state).bind(json).bind(now).bind(previous.duplicate_receipt_of.map(|id| id.to_string()))
                                .bind(invocation_id.to_string()).execute(&mut *tx).await?;
                            previous.state = if receipt.completed { InvocationState::Completed } else { InvocationState::Unknown };
                            previous.receipt = Some(receipt);
                            previous.finished_at_ms = Some(now);
                        }
                        ExecutionResponse::Invocation(previous)
                    }
                };
                tx.commit().await?;
                Ok(response)
            }
        }
    };
}
impl_invocations!(
    crate::SqliteJournalStore,
    "BEGIN IMMEDIATE",
    "",
    "SELECT CAST(strftime('%s', 'now') AS INTEGER) * 1000 + CAST(substr(strftime('%f', 'now'), 4, 3) AS INTEGER)"
);
impl_invocations!(
    crate::PostgresJournalStore,
    "BEGIN",
    " FOR UPDATE",
    "SELECT CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)"
);
