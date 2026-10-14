//! Durable control-plane records, separate from append-only conversation history.
//!
//! Acceptance commits before a caller may dispatch. A takeover fences the old
//! writer and marks its unfinished turn unknown, including accepted-but-not-yet-
//! dispatched turns. Nothing here replays prompts, resumes tools, or proves that
//! an old writer's external effects have stopped. Terminal writes require the
//! caller to have independently established containment/quiescence.

use chaos_ipc::ProcessId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::JournalError;

pub const MAX_EXECUTION_TURNS: i64 = 256;
pub const MAX_EXECUTION_OUTCOME_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSessionState {
    Idle,
    Active,
    Unknown,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTurnState {
    Accepted,
    Running,
    Completed,
    Failed,
    Interrupted,
    TimedOut,
    Unknown,
}

impl ExecutionTurnState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::TimedOut => "timed_out",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Interrupted | Self::TimedOut
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSession {
    pub session_id: Uuid,
    /// Server-selected configuration/tenant namespace, not caller billing input.
    pub scope: String,
    pub process_id: Option<ProcessId>,
    pub state: ExecutionSessionState,
    pub active_turn_id: Option<Uuid>,
    pub turn_count: u64,
    /// Monotonic quarantine incarnation; stale containment assertions conflict.
    pub recovery_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionLease {
    pub session_id: Uuid,
    pub owner_id: String,
    pub token: Uuid,
    /// Storage-clock deadline; never evidence of process termination.
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionTurn {
    pub turn_id: Uuid,
    /// Lowercase SHA-256 of the normalized execution payload.
    pub fingerprint: String,
    pub state: ExecutionTurnState,
    pub outcome: Option<Value>,
}

/// Operator assertion, not a storage proof that external effects stopped.
/// Resolution always closes the session and marks its active turn interrupted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResolution {
    pub resolution_id: Uuid,
    pub expected_process_id: Option<ProcessId>,
    pub expected_turn_id: Option<Uuid>,
    pub expected_recovery_epoch: u64,
    pub containment_confirmed: bool,
    pub actor: String,
    pub reason: String,
    pub outcome: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionResolutionRecord {
    pub request: ExecutionResolution,
    pub resolved_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionRequest {
    Invocation {
        request: ExecutionInvocationRequest,
    },
    Delivery {
        request: ExecutionDeliveryRequest,
    },
    /// Bind only an empty namespace. Legacy history is never auto-adopted.
    BindScope {
        scope: String,
        policy_digest: String,
    },
    GetScope {
        scope: String,
    },
    ClaimBound {
        session_id: Uuid,
        scope: String,
        policy_digest: String,
        owner_id: String,
        ttl_ms: u64,
    },
    /// Create or acquire an expired/unowned session. Retrying a live claim by the
    /// same owner returns the existing token; it does not renew or rotate it.
    Claim {
        session_id: Uuid,
        scope: String,
        owner_id: String,
        ttl_ms: u64,
    },
    Heartbeat {
        lease: ExecutionLease,
        ttl_ms: u64,
    },
    /// Bind once, before admission. Live/unknown process mappings are unique.
    BindProcess {
        lease: ExecutionLease,
        process_id: ProcessId,
    },
    /// Register a freshly allocated kernel identity, not caller-supplied resume
    /// history. Resuming in a bound scope requires an existing matching binding.
    BindNewProcess {
        lease: ExecutionLease,
        process_id: ProcessId,
    },
    /// Persist fingerprint and accepted state. A same-payload retry returns the
    /// old record even when closed or unknown; it never authorizes redispatch.
    Accept {
        lease: ExecutionLease,
        turn_id: Uuid,
        fingerprint: String,
    },
    AcceptMetered {
        lease: ExecutionLease,
        turn_id: Uuid,
        fingerprint: String,
    },
    /// Must commit before submitting to a kernel or external executor.
    Dispatch {
        lease: ExecutionLease,
        turn_id: Uuid,
    },
    /// Unknown retains the active reservation. A terminal outcome releases it
    /// only after this transaction commits. Identical terminal retries are safe.
    Finish {
        lease: ExecutionLease,
        turn_id: Uuid,
        state: ExecutionTurnState,
        outcome: Option<Value>,
    },
    /// Quarantine the entire process, including autonomous work without an HTTP
    /// turn. This is not a terminal outcome or evidence of containment.
    Quarantine {
        lease: ExecutionLease,
    },
    /// Only after independent containment. An active unknown turn must instead
    /// be resolved with Finish so its idempotency outcome is not discarded.
    ReconcileIdle {
        lease: ExecutionLease,
    },
    /// Closing/releasing an unfinished session is forbidden.
    Close {
        lease: ExecutionLease,
    },
    Release {
        lease: ExecutionLease,
    },
    /// Atomic audited closure after independent containment. Identical retries
    /// are read-only even after lease release; changed resolutions conflict.
    Resolve {
        lease: ExecutionLease,
        resolution: ExecutionResolution,
    },
    GetResolution {
        session_id: Uuid,
    },
    GetSession {
        session_id: Uuid,
    },
    GetTurn {
        session_id: Uuid,
        turn_id: Uuid,
    },
    ListSessions {
        scope: String,
        after: Option<Uuid>,
        limit: u32,
    },
}

impl ExecutionRequest {
    pub(crate) fn session_id(&self) -> Option<Uuid> {
        match self {
            Self::Claim { session_id, .. }
            | Self::ClaimBound { session_id, .. }
            | Self::GetSession { session_id }
            | Self::GetResolution { session_id }
            | Self::GetTurn { session_id, .. } => Some(*session_id),
            Self::Heartbeat { lease, .. }
            | Self::BindProcess { lease, .. }
            | Self::BindNewProcess { lease, .. }
            | Self::Accept { lease, .. }
            | Self::AcceptMetered { lease, .. }
            | Self::Dispatch { lease, .. }
            | Self::Finish { lease, .. }
            | Self::Quarantine { lease }
            | Self::ReconcileIdle { lease }
            | Self::Close { lease }
            | Self::Release { lease } => Some(lease.session_id),
            Self::Resolve { lease, .. } => Some(lease.session_id),
            Self::ListSessions { .. }
            | Self::BindScope { .. }
            | Self::GetScope { .. }
            | Self::Delivery { .. } => None,
            Self::Invocation { .. } => None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), JournalError> {
        match self {
            Self::Invocation { request } => request.validate()?,
            Self::Delivery { request } => request.validate()?,
            Self::BindScope {
                scope,
                policy_digest,
            } => {
                validate_name(scope)?;
                validate_digest(policy_digest)?;
            }
            Self::GetScope { scope } => validate_name(scope)?,
            Self::ClaimBound {
                scope,
                owner_id,
                ttl_ms,
                policy_digest,
                ..
            } => {
                validate_name(scope)?;
                validate_name(owner_id)?;
                validate_ttl(*ttl_ms)?;
                validate_digest(policy_digest)?;
            }
            Self::Claim {
                scope,
                owner_id,
                ttl_ms,
                ..
            } => {
                validate_name(scope)?;
                validate_name(owner_id)?;
                validate_ttl(*ttl_ms)?;
            }
            Self::Heartbeat { ttl_ms, .. } => validate_ttl(*ttl_ms)?,
            Self::Accept { fingerprint, .. } | Self::AcceptMetered { fingerprint, .. }
                if fingerprint.len() != 64
                    || !fingerprint
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
            {
                return Err(invalid("fingerprint must be lowercase SHA-256 hex"));
            }
            Self::Finish { state, outcome, .. } => {
                if !state.is_terminal() && *state != ExecutionTurnState::Unknown {
                    return Err(invalid("finish requires a terminal or unknown outcome"));
                }
                if let Some(outcome) = outcome
                    && serde_json::to_vec(outcome)
                        .map_err(|source| JournalError::Serialize {
                            field: "execution outcome",
                            source,
                        })?
                        .len()
                        > MAX_EXECUTION_OUTCOME_BYTES
                {
                    return Err(invalid("execution outcome exceeds size limit"));
                }
            }
            Self::ListSessions { scope, limit, .. } => {
                validate_name(scope)?;
                if !(1..=256).contains(limit) {
                    return Err(invalid("execution page limit must be 1..256"));
                }
            }
            Self::Resolve { resolution, .. } => resolution.validate()?,
            _ => {}
        }
        Ok(())
    }
}

fn validate_digest(value: &str) -> Result<(), JournalError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("policy digest must be lowercase SHA-256 hex"));
    }
    Ok(())
}

impl ExecutionResolution {
    pub fn validate(&self) -> Result<(), JournalError> {
        if !self.containment_confirmed {
            return Err(invalid("independent containment confirmation is required"));
        }
        validate_name(&self.actor)?;
        if self.reason.trim().is_empty() || self.reason.len() > 4096 {
            return Err(invalid("resolution reason must contain 1..4096 bytes"));
        }
        if self.expected_turn_id.is_none() && self.outcome.is_some() {
            return Err(invalid(
                "a no-turn resolution cannot contain a turn outcome",
            ));
        }
        if let Some(outcome) = &self.outcome
            && serde_json::to_vec(outcome)
                .map_err(|source| JournalError::Serialize {
                    field: "execution resolution outcome",
                    source,
                })?
                .len()
                > MAX_EXECUTION_OUTCOME_BYTES
        {
            return Err(invalid("execution resolution outcome exceeds size limit"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "result", rename_all = "snake_case")]
pub enum ExecutionResponse {
    InvocationBegan {
        invocation: ExecutionInvocation,
        newly_recorded: bool,
    },
    Invocation(ExecutionInvocation),
    Accounting(ExecutionAccounting),
    DeliveryRecorded,
    Delivery(Option<ExecutionDelivery>),
    Deliveries(Vec<ExecutionDeliveryStatus>),
    Claimed {
        session: ExecutionSession,
        lease: ExecutionLease,
    },
    Lease(ExecutionLease),
    Session(ExecutionSession),
    Accepted {
        turn: ExecutionTurn,
        newly_accepted: bool,
    },
    Dispatched {
        turn: ExecutionTurn,
        /// Only a true value authorizes the initial dispatch. A lost response
        /// is uncertain; a retry returning false must never resubmit execution.
        newly_dispatched: bool,
    },
    Turn(ExecutionTurn),
    Sessions(Vec<ExecutionSession>),
    Released,
    Resolved(ExecutionResolutionRecord),
    Resolution(Option<ExecutionResolutionRecord>),
    Scope(Option<String>),
}

fn validate_name(value: &str) -> Result<(), JournalError> {
    if value.trim().is_empty() || value.len() > 256 {
        return Err(invalid("execution scope/owner must contain 1..256 bytes"));
    }
    Ok(())
}

fn validate_ttl(ttl_ms: u64) -> Result<(), JournalError> {
    if !(1_000..=300_000).contains(&ttl_ms) {
        return Err(invalid("execution lease TTL must be 1000..300000ms"));
    }
    Ok(())
}

pub(crate) fn invalid(message: &str) -> JournalError {
    JournalError::InvalidRequest(message.to_owned())
}

pub(crate) fn conflict(message: &str) -> JournalError {
    JournalError::ExecutionConflict(message.to_owned())
}

pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, JournalError> {
    serde_json::from_str(value).map_err(|source| JournalError::Deserialize {
        field: "execution record",
        source,
    })
}

pub(crate) fn decode_state<T: serde::de::DeserializeOwned>(
    value: String,
) -> Result<T, JournalError> {
    serde_json::from_value(Value::String(value)).map_err(|source| JournalError::Deserialize {
        field: "execution state",
        source,
    })
}

pub(crate) fn parse_uuid(value: String) -> Result<Uuid, JournalError> {
    Uuid::parse_str(&value).map_err(|source| JournalError::InvalidProcessId { value, source })
}

// Both backends run the same state machine and parameterized SQL. Only writer
// locking and the authoritative storage-clock expression differ. SQLite holds
// BEGIN IMMEDIATE; PostgreSQL locks the session row FOR UPDATE.
mod invocations;
mod outbox;
pub use invocations::{
    ExecutionAccounting, ExecutionInvocation, ExecutionInvocationRequest, InvocationPurpose,
    InvocationReceipt, InvocationStart, InvocationState, InvocationSummary, InvocationTokenUsage,
    MAX_EXECUTION_INVOCATIONS,
};
mod storage;
pub use outbox::{
    ExecutionDelivery, ExecutionDeliveryLease, ExecutionDeliveryRequest, ExecutionDeliveryStatus,
    ExecutionEvent,
};

#[cfg(test)]
mod tests;
