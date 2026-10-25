use chaos_ipc::ProcessId;
use sqlx::{Row, SqlitePool};

use crate::error::JournalError;
use crate::lease_policy::validate_lease;
use crate::model::{EntrySeq, Lease, OwnerId};
use crate::store::timestamp_from_epoch_seconds;

pub(super) async fn ensure_process_exists(
    pool: &SqlitePool,
    process_id: &ProcessId,
) -> Result<(), JournalError> {
    let exists = sqlx::query("SELECT 1 FROM processes WHERE id = ?")
        .bind(process_id.to_string())
        .fetch_optional(pool)
        .await?;
    if exists.is_some() {
        return Ok(());
    }
    Err(JournalError::ProcessNotFound(*process_id))
}

pub(super) async fn ensure_process_exists_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    process_id: &ProcessId,
) -> Result<(), JournalError> {
    let exists = sqlx::query("SELECT 1 FROM processes WHERE id = ?")
        .bind(process_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_some() {
        return Ok(());
    }
    Err(JournalError::ProcessNotFound(*process_id))
}

pub(super) async fn load_valid_lease_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    process_id: &ProcessId,
    owner_id: &OwnerId,
    lease_token: &str,
    now: jiff::Timestamp,
) -> Result<Lease, JournalError> {
    let row = sqlx::query(
        "SELECT owner_id, lease_token, expires_at
         FROM process_leases
         WHERE process_id = ?",
    )
    .bind(process_id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    validate_lease_row(row, process_id, owner_id, lease_token, now)
}

fn validate_lease_row(
    row: Option<sqlx::sqlite::SqliteRow>,
    process_id: &ProcessId,
    owner_id: &OwnerId,
    lease_token: &str,
    now: jiff::Timestamp,
) -> Result<Lease, JournalError> {
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
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    process_id: &ProcessId,
) -> Result<EntrySeq, JournalError> {
    let row = sqlx::query(
        "SELECT COALESCE(MAX(seq) + 1, 0) AS next_seq FROM journal_entries WHERE process_id = ?",
    )
    .bind(process_id.to_string())
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.get("next_seq"))
}
