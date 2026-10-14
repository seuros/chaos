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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionRequest {
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
    /// Persist fingerprint and accepted state. A same-payload retry returns the
    /// old record even when closed or unknown; it never authorizes redispatch.
    Accept {
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
            | Self::GetSession { session_id }
            | Self::GetTurn { session_id, .. } => Some(*session_id),
            Self::Heartbeat { lease, .. }
            | Self::BindProcess { lease, .. }
            | Self::Accept { lease, .. }
            | Self::Dispatch { lease, .. }
            | Self::Finish { lease, .. }
            | Self::Quarantine { lease }
            | Self::ReconcileIdle { lease }
            | Self::Close { lease }
            | Self::Release { lease } => Some(lease.session_id),
            Self::ListSessions { .. } => None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), JournalError> {
        match self {
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
            Self::Accept { fingerprint, .. }
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
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "result", rename_all = "snake_case")]
pub enum ExecutionResponse {
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
mod storage;

#[cfg(test)]
mod tests;
