use std::time::Duration;

use chaos_ipc::ProcessId;
use sqlx::{Row, SqlitePool};

use crate::error::JournalError;
use crate::lease_policy::{LeaseRow, column};
use crate::model::EntrySeq;

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

pub(super) async fn load_lease_row_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    process_id: &ProcessId,
) -> Result<Option<LeaseRow>, JournalError> {
    let row = sqlx::query(
        "SELECT owner_id, lease_token, last_issued, expires_at
         FROM process_leases
         WHERE process_id = ?",
    )
    .bind(process_id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|row| lease_row(process_id, &row)).transpose()
}

fn lease_row(
    process_id: &ProcessId,
    row: &sqlx::sqlite::SqliteRow,
) -> Result<LeaseRow, JournalError> {
    let resource = process_id.to_string();
    Ok(LeaseRow {
        owner_id: row.get("owner_id"),
        token: row
            .get::<Option<i64>, _>("lease_token")
            .map(|token| column(&resource, "lease_token", token))
            .transpose()?,
        last_issued: column(&resource, "last_issued", row.get("last_issued"))?,
        expires_at: Duration::from_secs(column(&resource, "expires_at", row.get("expires_at"))?),
    })
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
