use chaos_ipc::ProcessId;
use jiff::Timestamp;

use crate::error::JournalError;
use crate::model::{Lease, OwnerId};

/// Acquisition may replace an expired lease or rotate the same owner's token.
pub(crate) fn ensure_acquirable(
    process_id: &ProcessId,
    owner_id: &OwnerId,
    existing: Option<(OwnerId, Timestamp)>,
    now: Timestamp,
) -> Result<(), JournalError> {
    if let Some((current_owner_id, expires_at)) = existing
        && expires_at > now
        && current_owner_id != *owner_id
    {
        return Err(JournalError::LeaseConflict {
            process_id: *process_id,
            current_owner_id,
            expires_at,
        });
    }
    Ok(())
}

/// Existing writers must present an unexpired lease with matching credentials.
pub(crate) fn validate_lease(
    lease: Option<Lease>,
    process_id: &ProcessId,
    owner_id: &OwnerId,
    lease_token: &str,
    now: Timestamp,
) -> Result<Lease, JournalError> {
    let Some(lease) = lease else {
        return Err(JournalError::InvalidLease {
            process_id: *process_id,
        });
    };
    // Expiry wins over credential mismatch, including exactly at `now`.
    if lease.expires_at <= now {
        return Err(JournalError::LeaseExpired {
            process_id: *process_id,
        });
    }
    if lease.owner_id != *owner_id || lease.lease_token != lease_token {
        return Err(JournalError::InvalidLease {
            process_id: *process_id,
        });
    }
    Ok(lease)
}

#[cfg(test)]
mod tests;
