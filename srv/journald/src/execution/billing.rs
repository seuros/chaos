//! Explicit operator evidence and versioned prices, never an inferred invoice.
use super::*;

pub const MAX_RECONCILIATIONS_PER_TURN: i64 = 512;

/// Nanocurrency units per `tokens_per_unit`. Input/cache and output/reasoning
/// buckets are disjoint: normalized aggregate counters must not be double priced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTokenPrices {
    pub uncached_input: u64,
    pub cache_creation_input: u64,
    pub cached_input: u64,
    pub ordinary_output: u64,
    pub reasoning_output: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPriceSchedule {
    pub version: String,
    pub provider_id: String,
    pub model: String,
    /// Explicitly observed/attested tier, not the request's preferred tier.
    pub service_tier: String,
    /// Three uppercase ASCII letters, not automatic FX conversion.
    pub currency: String,
    pub tokens_per_unit: u64,
    pub prices: ExecutionTokenPrices,
    pub source_sha256: String,
    pub actor: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionMoney {
    pub currency: String,
    /// Exact nonnegative integer in 1e-9 currency units, serialized as a string.
    pub nanounits: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionReconciliation {
    pub reconciliation_id: Uuid,
    pub invocation_id: Uuid,
    /// One external provider receipt/attempt, not a logical invocation ID.
    pub provider_receipt_id: String,
    pub evidence_sha256: String,
    pub model: String,
    pub service_tier: String,
    pub usage: Option<InvocationTokenUsage>,
    pub charged: Option<ExecutionMoney>,
    pub price_version: Option<String>,
    pub actor: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReconciliationRecord {
    pub request: ExecutionReconciliation,
    pub estimated: Option<ExecutionMoney>,
    pub recorded_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionBillingReport {
    pub billing_complete: bool,
    pub records: Vec<ExecutionReconciliationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionBillingRequest {
    PutPrice {
        scope: String,
        schedule: ExecutionPriceSchedule,
    },
    GetPrice {
        scope: String,
        version: String,
    },
    Reconcile {
        scope: String,
        session_id: Uuid,
        turn_id: Uuid,
        record: ExecutionReconciliation,
    },
    Read {
        scope: String,
        session_id: Uuid,
        turn_id: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "result", rename_all = "snake_case")]
pub enum ExecutionBillingResponse {
    Price(ExecutionPriceSchedule),
    Reconciled(ExecutionReconciliationRecord),
    Report(ExecutionBillingReport),
}

fn evidence_actor(actor: &str, reason: &str) -> Result<(), JournalError> {
    validate_name(actor)?;
    if reason.trim().is_empty() || reason.len() > 4096 {
        return Err(invalid("billing evidence requires a bounded reason"));
    }
    Ok(())
}

fn currency(value: &str) -> Result<(), JournalError> {
    if value.len() != 3 || !value.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(invalid("currency requires three uppercase ASCII letters"));
    }
    Ok(())
}

impl ExecutionPriceSchedule {
    fn validate(&self) -> Result<(), JournalError> {
        for name in [
            &self.version,
            &self.provider_id,
            &self.model,
            &self.service_tier,
        ] {
            validate_name(name)?;
        }
        currency(&self.currency)?;
        validate_digest(&self.source_sha256)?;
        evidence_actor(&self.actor, &self.reason)?;
        if !(1..=1_000_000_000).contains(&self.tokens_per_unit) {
            return Err(invalid("price token unit outside 1..1e9"));
        }
        Ok(())
    }

    pub fn estimate(&self, usage: &InvocationTokenUsage) -> Result<ExecutionMoney, JournalError> {
        self.validate()?;
        usage.validate()?;
        let buckets = [
            (
                usage.input_tokens - usage.cache_creation_input_tokens - usage.cached_input_tokens,
                self.prices.uncached_input,
            ),
            (
                usage.cache_creation_input_tokens,
                self.prices.cache_creation_input,
            ),
            (usage.cached_input_tokens, self.prices.cached_input),
            (
                usage.output_tokens - usage.reasoning_output_tokens,
                self.prices.ordinary_output,
            ),
            (usage.reasoning_output_tokens, self.prices.reasoning_output),
        ];
        let total = buckets.into_iter().try_fold(0u128, |sum, (tokens, rate)| {
            (tokens as u128)
                .checked_mul(u128::from(rate))
                .and_then(|value| sum.checked_add(value))
                .ok_or_else(|| invalid("price arithmetic overflow"))
        })?;
        let unit = u128::from(self.tokens_per_unit);
        // Round once, upward to one nanocurrency unit, not once per bucket.
        let amount = total / unit + u128::from(total % unit != 0);
        Ok(ExecutionMoney {
            currency: self.currency.clone(),
            nanounits: amount.to_string(),
        })
    }
}

impl ExecutionReconciliation {
    fn validate(&self) -> Result<(), JournalError> {
        for name in [&self.provider_receipt_id, &self.model, &self.service_tier] {
            validate_name(name)?;
        }
        validate_digest(&self.evidence_sha256)?;
        evidence_actor(&self.actor, &self.reason)?;
        if self.usage.is_none() && self.charged.is_none() {
            return Err(invalid(
                "reconciliation requires observed usage or charged amount",
            ));
        }
        if let Some(usage) = &self.usage {
            usage.validate()?;
        }
        if let Some(money) = &self.charged {
            currency(&money.currency)?;
            let amount = money
                .nanounits
                .parse::<u128>()
                .map_err(|_| invalid("charged amount must be an exact nonnegative integer"))?;
            if amount.to_string() != money.nanounits {
                return Err(invalid("charged amount must be canonical"));
            }
        }
        if let Some(version) = &self.price_version {
            validate_name(version)?;
            if self.usage.is_none() {
                return Err(invalid("a price estimate requires observed usage"));
            }
        }
        Ok(())
    }
}

impl ExecutionBillingRequest {
    pub fn validate(&self) -> Result<(), JournalError> {
        let scope = match self {
            Self::PutPrice { scope, schedule } => {
                schedule.validate()?;
                scope
            }
            Self::GetPrice { scope, version } => {
                validate_name(version)?;
                scope
            }
            Self::Reconcile { scope, record, .. } => {
                record.validate()?;
                scope
            }
            Self::Read { scope, .. } => scope,
        };
        validate_name(scope)
    }
}

macro_rules! impl_billing {
    ($store:ty, $begin:expr, $lock:literal, $clock:literal) => {
        impl $store {
            pub(crate) async fn execute_billing(&self, request: ExecutionBillingRequest)
                -> Result<ExecutionBillingResponse, JournalError> {
                request.validate()?;
                let mut tx = self.pool().begin_with($begin).await?;
                let now: i64 = sqlx::query_scalar($clock).fetch_one(&mut *tx).await?;
                let response = match request {
                    ExecutionBillingRequest::PutPrice { scope, schedule } => {
                        let bound: Option<String> = sqlx::query_scalar(concat!(
                            "SELECT scope FROM execution_scopes WHERE scope = $1", $lock
                        )).bind(&scope).fetch_optional(&mut *tx).await?;
                        if bound.is_none() { return Err(JournalError::ExecutionNotFound); }
                        let json = serde_json::to_string(&schedule).map_err(|source|
                            JournalError::Serialize { field: "price schedule", source })?;
                        sqlx::query("INSERT INTO execution_price_schedules
                            (scope, version, schedule_json, created_at_ms) VALUES ($1,$2,$3,$4)
                            ON CONFLICT(scope,version) DO NOTHING")
                            .bind(&scope).bind(&schedule.version).bind(&json).bind(now)
                            .execute(&mut *tx).await?;
                        let saved: String = sqlx::query_scalar("SELECT schedule_json FROM execution_price_schedules WHERE scope = $1 AND version = $2")
                            .bind(&scope).bind(&schedule.version).fetch_one(&mut *tx).await?;
                        if decode::<ExecutionPriceSchedule>(&saved)? != schedule {
                            return Err(conflict("price schedule version is immutable"));
                        }
                        ExecutionBillingResponse::Price(schedule)
                    }
                    ExecutionBillingRequest::GetPrice { scope, version } => {
                        let saved: String = sqlx::query_scalar("SELECT schedule_json FROM execution_price_schedules WHERE scope = $1 AND version = $2")
                            .bind(&scope).bind(&version).fetch_optional(&mut *tx).await?
                            .ok_or(JournalError::ExecutionNotFound)?;
                        ExecutionBillingResponse::Price(decode(&saved)?)
                    }
                    ExecutionBillingRequest::Read { scope, session_id, turn_id } => {
                        let session_id = session_id.to_string();
                        let turn_id = turn_id.to_string();
                        let owner: Option<String> = sqlx::query_scalar(concat!(
                            "SELECT scope FROM execution_sessions WHERE session_id = $1", $lock
                        )).bind(&session_id).fetch_optional(&mut *tx).await?;
                        if owner.as_deref() != Some(scope.as_str()) { return Err(JournalError::ExecutionNotFound); }
                        let present: Option<String> = sqlx::query_scalar("SELECT turn_id FROM execution_turns WHERE session_id = $1 AND turn_id = $2")
                            .bind(&session_id).bind(&turn_id).fetch_optional(&mut *tx).await?;
                        if present.is_none() { return Err(JournalError::ExecutionNotFound); }
                        let rows: Vec<String> = sqlx::query_scalar("SELECT record_json FROM execution_reconciliations
                            WHERE session_id = $1 AND turn_id = $2 ORDER BY created_at_ms, reconciliation_id")
                            .bind(&session_id).bind(&turn_id).fetch_all(&mut *tx).await?;
                        ExecutionBillingResponse::Report(ExecutionBillingReport {
                            billing_complete: false,
                            records: rows.iter().map(|row| decode(row)).collect::<Result<_,_>>()?,
                        })
                    }
                    ExecutionBillingRequest::Reconcile { scope, session_id, turn_id, record } => {
                        let session_id = session_id.to_string();
                        let turn_id = turn_id.to_string();
                        let owner: Option<String> = sqlx::query_scalar(concat!(
                            "SELECT scope FROM execution_sessions WHERE session_id = $1", $lock
                        )).bind(&session_id).fetch_optional(&mut *tx).await?;
                        if owner.as_deref() != Some(scope.as_str()) { return Err(JournalError::ExecutionNotFound); }
                        let original: Option<String> = sqlx::query_scalar("SELECT record_json FROM execution_reconciliations WHERE reconciliation_id = $1")
                            .bind(record.reconciliation_id.to_string()).fetch_optional(&mut *tx).await?;
                        if let Some(original) = original {
                            let saved: ExecutionReconciliationRecord = decode(&original)?;
                            let identity: (String,String) = sqlx::query_as("SELECT session_id, turn_id FROM execution_reconciliations WHERE reconciliation_id = $1")
                                .bind(record.reconciliation_id.to_string()).fetch_one(&mut *tx).await?;
                            if saved.request != record || identity != (session_id, turn_id) {
                                return Err(conflict("reconciliation identity is immutable"));
                            }
                            tx.commit().await?;
                            return Ok(ExecutionBillingResponse::Reconciled(saved));
                        }
                        let sealed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_archives
                            WHERE session_id = $1 AND sealed_at_ms IS NOT NULL")
                            .bind(&session_id).fetch_one(&mut *tx).await?;
                        if sealed != 0 {
                            return Err(conflict("sealed archive cannot accept new financial evidence"));
                        }
                        let invocation_json: String = sqlx::query_scalar("SELECT request_json FROM execution_invocations
                            WHERE invocation_id = $1 AND session_id = $2 AND turn_id = $3")
                            .bind(record.invocation_id.to_string()).bind(&session_id).bind(&turn_id)
                            .fetch_optional(&mut *tx).await?.ok_or(JournalError::ExecutionNotFound)?;
                        let invocation: InvocationStart = decode(&invocation_json)?;
                        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_reconciliations WHERE session_id = $1 AND turn_id = $2")
                            .bind(&session_id).bind(&turn_id).fetch_one(&mut *tx).await?;
                        if count >= MAX_RECONCILIATIONS_PER_TURN { return Err(conflict("reconciliation limit reached")); }
                        let estimated = if let Some(version) = &record.price_version {
                            let json: String = sqlx::query_scalar("SELECT schedule_json FROM execution_price_schedules WHERE scope = $1 AND version = $2")
                                .bind(&scope).bind(version).fetch_optional(&mut *tx).await?.ok_or(JournalError::ExecutionNotFound)?;
                            let schedule: ExecutionPriceSchedule = decode(&json)?;
                            if schedule.provider_id != invocation.provider_id || schedule.model != record.model
                                || schedule.service_tier != record.service_tier
                                || record.charged.as_ref().is_some_and(|money| money.currency != schedule.currency)
                            { return Err(conflict("price schedule does not match provider/model/tier/currency evidence")); }
                            Some(schedule.estimate(record.usage.as_ref().ok_or_else(|| invalid("price usage missing"))?)?)
                        } else { None };
                        let key = serde_json::to_string(&("external-provider-receipt/v1", &scope, &invocation.provider_id, &record.provider_receipt_id))
                            .map_err(|source| JournalError::Serialize { field: "external receipt identity", source })?;
                        let saved = ExecutionReconciliationRecord { request: record, estimated, recorded_at_ms: now };
                        let json = serde_json::to_string(&saved).map_err(|source|
                            JournalError::Serialize { field: "reconciliation", source })?;
                        let changed = sqlx::query("INSERT INTO execution_reconciliations
                            (reconciliation_id, scope, session_id, turn_id, invocation_id, receipt_key, record_json, created_at_ms)
                            VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING")
                            .bind(saved.request.reconciliation_id.to_string()).bind(scope).bind(session_id)
                            .bind(turn_id).bind(saved.request.invocation_id.to_string()).bind(key).bind(json).bind(now)
                            .execute(&mut *tx).await?.rows_affected();
                        if changed != 1 { return Err(conflict("external receipt already reconciled")); }
                        ExecutionBillingResponse::Reconciled(saved)
                    }
                };
                tx.commit().await?;
                Ok(response)
            }
        }
    };
}
impl_billing!(
    crate::SqliteJournalStore,
    "BEGIN IMMEDIATE",
    "",
    "SELECT CAST(strftime('%s', 'now') AS INTEGER) * 1000 + CAST(substr(strftime('%f', 'now'), 4, 3) AS INTEGER)"
);
impl_billing!(
    crate::PostgresJournalStore,
    "BEGIN",
    " FOR UPDATE",
    "SELECT CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)"
);
