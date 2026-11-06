use std::time::Duration;

use chaos_ipc::ProcessId;
use sqlx::{Postgres, Row, Transaction};

use super::codec::serialize_item;
use crate::error::JournalError;
use crate::lease_policy::{LeaseRow, column};
use crate::model::{EntrySeq, JournalEntry};
use crate::store::journal_item_type;

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

pub(super) async fn load_lease_row_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    process_id: &ProcessId,
) -> Result<Option<LeaseRow>, JournalError> {
    let row = sqlx::query(
        "SELECT owner_id, lease_token, last_issued, expires_at
         FROM process_leases
         WHERE process_id = $1
         FOR UPDATE",
    )
    .bind(process_id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    let resource = process_id.to_string();
    row.map(|row| {
        Ok(LeaseRow {
            owner_id: row.get("owner_id"),
            token: row
                .get::<Option<i64>, _>("lease_token")
                .map(|token| column(&resource, "lease_token", token))
                .transpose()?,
            last_issued: column(&resource, "last_issued", row.get("last_issued"))?,
            expires_at: Duration::from_secs(column(
                &resource,
                "expires_at",
                row.get("expires_at"),
            )?),
        })
    })
    .transpose()
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
