use chaos_ipc::ProcessId;
use sqlx::{Postgres, Row, Transaction};

use super::codec::serialize_item;
use crate::error::JournalError;
use crate::lease_policy::validate_lease;
use crate::model::{EntrySeq, JournalEntry, Lease, OwnerId};
use crate::store::{journal_item_type, timestamp_from_epoch_seconds};

pub(super) async fn lock_process(
    tx: &mut Transaction<'_, Postgres>,
    process_id: &ProcessId,
) -> Result<(), JournalError> {
    let row = sqlx::query("SELECT 1 FROM processes WHERE id = $1 FOR UPDATE")
        .bind(process_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
    if row.is_none() {
        return Err(JournalError::ProcessNotFound(*process_id));
    }
    Ok(())
}

pub(super) async fn load_valid_lease_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    process_id: &ProcessId,
    owner_id: &OwnerId,
    lease_token: &str,
    now: jiff::Timestamp,
) -> Result<Lease, JournalError> {
    let row = sqlx::query(
        "SELECT owner_id, lease_token, expires_at
         FROM process_leases
         WHERE process_id = $1
         FOR UPDATE",
    )
    .bind(process_id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    let lease = match row {
        Some(row) => Some(Lease {
            process_id: *process_id,
            owner_id: row.get("owner_id"),
            lease_token: row.get("lease_token"),
            expires_at: timestamp_from_epoch_seconds(row.get("expires_at"))?,
        }),
        None => None,
    };
    validate_lease(lease, process_id, owner_id, lease_token, now)
}

pub(super) async fn next_seq_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    process_id: &ProcessId,
) -> Result<EntrySeq, JournalError> {
    let row = sqlx::query(
        "SELECT COALESCE(MAX(seq) + 1, 0) AS next_seq
         FROM journal_entries
         WHERE process_id = $1",
    )
    .bind(process_id.to_string())
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.get("next_seq"))
}

pub(super) async fn insert_journal_entry(
    tx: &mut Transaction<'_, Postgres>,
    process_id: &ProcessId,
    entry: &JournalEntry,
) -> Result<(), JournalError> {
    sqlx::query(
        "INSERT INTO journal_entries (
            process_id,
            seq,
            recorded_at,
            item_type,
            payload_json
         ) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(process_id.to_string())
    .bind(entry.seq)
    .bind(entry.recorded_at.as_second())
    .bind(journal_item_type(&entry.item))
    .bind(serialize_item(&entry.item)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
