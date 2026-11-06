use std::time::Duration;

use chaos_ipc::ProcessId;
use lease_machines::{
    Checkpoint, ClockPolicy, Decision, FencingToken, Lease, LeaseAuthority, OwnerId, Rejection,
};

use crate::error::JournalError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeaseRow {
    pub(crate) owner_id: String,
    pub(crate) token: Option<u64>,
    pub(crate) last_issued: u64,
    pub(crate) expires_at: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Grant {
    pub(crate) token: u64,
    pub(crate) last_issued: u64,
    pub(crate) expires_at: Duration,
}

impl From<Lease> for Grant {
    fn from(lease: Lease) -> Self {
        Self {
            token: lease.token().value(),
            last_issued: lease.token().value(),
            expires_at: lease.expires_at(),
        }
    }
}

/// Stable FNV-1a digest; the authority only compares owners, the row keeps the string.
pub(crate) fn owner(owner_id: &str) -> OwnerId {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in owner_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    OwnerId::new(hash)
}

pub(crate) fn token(value: &str) -> Option<FencingToken> {
    value.parse().ok().and_then(FencingToken::new)
}

pub(crate) fn column(resource: &str, name: &str, value: i64) -> Result<u64, JournalError> {
    u64::try_from(value).map_err(|_| JournalError::LeaseHistory {
        resource: resource.to_owned(),
        message: format!("{name} is negative: {value}"),
    })
}

pub(crate) fn signed(resource: &str, name: &str, value: u64) -> Result<i64, JournalError> {
    i64::try_from(value).map_err(|_| JournalError::LeaseHistory {
        resource: resource.to_owned(),
        message: format!("{name} overflows storage: {value}"),
    })
}

pub(crate) fn wall_clock(now: jiff::Timestamp) -> Result<Duration, JournalError> {
    let millis = now.as_millisecond();
    u64::try_from(millis)
        .map(Duration::from_millis)
        .map_err(|_| JournalError::InvalidTimestamp {
            value: millis.to_string(),
            message: "wall clock precedes the Unix epoch".to_owned(),
        })
}

pub(crate) fn epoch_millis(at: Duration) -> Result<i64, JournalError> {
    i64::try_from(at.as_millis()).map_err(|_| JournalError::InvalidTimestamp {
        value: at.as_millis().to_string(),
        message: "timestamp overflow".to_owned(),
    })
}

pub(crate) fn epoch_seconds(at: Duration) -> Result<i64, JournalError> {
    i64::try_from(at.as_secs()).map_err(|_| JournalError::InvalidTimestamp {
        value: at.as_secs().to_string(),
        message: "timestamp overflow".to_owned(),
    })
}

pub(crate) struct ResourceLease {
    authority: LeaseAuthority,
}

impl ResourceLease {
    pub(crate) fn restore(resource: &str, row: Option<&LeaseRow>) -> Result<Self, JournalError> {
        let Some(row) = row else {
            return Ok(Self {
                authority: LeaseAuthority::new().with_clock_policy(ClockPolicy::Clamp),
            });
        };
        let never_issued = || JournalError::LeaseHistory {
            resource: resource.to_owned(),
            message: format!(
                "token {:?} was never issued by high-water {}",
                row.token, row.last_issued
            ),
        };
        let lease = match row.token {
            None => None,
            Some(token) => Some(Lease::new(
                owner(&row.owner_id),
                FencingToken::new(token).ok_or_else(never_issued)?,
                row.expires_at,
            )),
        };
        let checkpoint =
            Checkpoint::from_parts(FencingToken::new(row.last_issued), lease, Duration::ZERO)
                .ok_or_else(never_issued)?;
        Ok(Self {
            authority: LeaseAuthority::restore(checkpoint).with_clock_policy(ClockPolicy::Clamp),
        })
    }

    pub(crate) fn acquire(&mut self, owner_id: &str, now: Duration, ttl: Duration) -> Decision {
        self.authority.acquire(owner(owner_id), now, ttl)
    }

    pub(crate) fn reacquire(&mut self, owner_id: &str, now: Duration, ttl: Duration) -> Decision {
        self.authority.reacquire(owner(owner_id), now, ttl)
    }

    pub(crate) fn renew(
        &mut self,
        owner_id: &str,
        token: &str,
        now: Duration,
        ttl: Duration,
    ) -> Decision {
        match self::token(token) {
            Some(token) => self.authority.renew(owner(owner_id), token, now, ttl),
            None => Decision::Rejected(Rejection::StaleToken),
        }
    }

    pub(crate) fn release(&mut self, owner_id: &str, token: &str, now: Duration) -> Decision {
        match self::token(token) {
            Some(token) => self.authority.release(owner(owner_id), token, now),
            None => Decision::Rejected(Rejection::StaleToken),
        }
    }

    /// Read-only credential check with the authority's own precedence.
    pub(crate) fn verify(&self, owner_id: &str, token: &str, now: Duration) -> Option<Rejection> {
        let Some(lease) = self.authority.checkpoint().lease() else {
            return Some(Rejection::NoLease);
        };
        if lease.owner() != owner(owner_id) {
            Some(Rejection::WrongOwner)
        } else if self::token(token) != Some(lease.token()) {
            Some(Rejection::StaleToken)
        } else if now >= lease.expires_at() {
            Some(Rejection::Expired)
        } else {
            None
        }
    }
}

fn timestamp(at: Duration) -> Result<jiff::Timestamp, JournalError> {
    let millis = epoch_millis(at)?;
    jiff::Timestamp::from_millisecond(millis).map_err(|err| JournalError::InvalidTimestamp {
        value: millis.to_string(),
        message: err.to_string(),
    })
}

fn process_error(
    rejection: Rejection,
    process_id: &ProcessId,
    row: Option<&LeaseRow>,
) -> JournalError {
    let invalid = || JournalError::InvalidLease {
        process_id: *process_id,
    };
    match rejection {
        Rejection::Occupied => match row {
            Some(row) => timestamp(row.expires_at).map_or_else(
                |error| error,
                |expires_at| JournalError::LeaseConflict {
                    process_id: *process_id,
                    current_owner_id: row.owner_id.clone(),
                    expires_at,
                },
            ),
            None => invalid(),
        },
        Rejection::Expired => JournalError::LeaseExpired {
            process_id: *process_id,
        },
        Rejection::NoLease | Rejection::WrongOwner | Rejection::StaleToken => invalid(),
        other => JournalError::InvalidRequest(format!("lease request rejected: {other:?}")),
    }
}

/// Acquisition may replace an expired lease or rotate the same owner's token.
pub(crate) fn acquire_process(
    process_id: &ProcessId,
    owner_id: &str,
    row: Option<&LeaseRow>,
    now: Duration,
    ttl: Duration,
) -> Result<Grant, JournalError> {
    let mut lease = ResourceLease::restore(&process_id.to_string(), row)?;
    match lease.reacquire(owner_id, now, ttl) {
        Decision::Granted(lease) => Ok(Grant::from(lease)),
        Decision::Rejected(rejection) => Err(process_error(rejection, process_id, row)),
        other => Err(JournalError::InvalidRequest(format!(
            "unexpected lease decision: {other:?}"
        ))),
    }
}

/// Renewal keeps the token and never shortens the current expiration.
pub(crate) fn renew_process(
    process_id: &ProcessId,
    owner_id: &str,
    token: &str,
    row: Option<&LeaseRow>,
    now: Duration,
    ttl: Duration,
) -> Result<Grant, JournalError> {
    let mut lease = ResourceLease::restore(&process_id.to_string(), row)?;
    match lease.renew(owner_id, token, now, ttl) {
        Decision::Renewed(lease) => Ok(Grant {
            last_issued: row.map_or(lease.token().value(), |row| row.last_issued),
            ..Grant::from(lease)
        }),
        Decision::Rejected(rejection) => Err(process_error(rejection, process_id, row)),
        other => Err(JournalError::InvalidRequest(format!(
            "unexpected lease decision: {other:?}"
        ))),
    }
}

pub(crate) fn release_process(
    process_id: &ProcessId,
    owner_id: &str,
    token: &str,
    row: Option<&LeaseRow>,
    now: Duration,
) -> Result<(), JournalError> {
    let mut lease = ResourceLease::restore(&process_id.to_string(), row)?;
    match lease.release(owner_id, token, now) {
        Decision::Released => Ok(()),
        Decision::Rejected(rejection) => Err(process_error(rejection, process_id, row)),
        other => Err(JournalError::InvalidRequest(format!(
            "unexpected lease decision: {other:?}"
        ))),
    }
}

/// Existing writers must present an unexpired lease with matching credentials.
pub(crate) fn verify_process(
    process_id: &ProcessId,
    owner_id: &str,
    token: &str,
    row: Option<&LeaseRow>,
    now: Duration,
) -> Result<(), JournalError> {
    let lease = ResourceLease::restore(&process_id.to_string(), row)?;
    match lease.verify(owner_id, token, now) {
        None => Ok(()),
        Some(rejection) => Err(process_error(rejection, process_id, row)),
    }
}

#[cfg(test)]
mod tests;
