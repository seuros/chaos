use std::path::Path;
use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_proc::open_runtime_db_at_path;
use chaos_proc::runtime_db_path;
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::JournalError;
use crate::lease_policy::ensure_acquirable;
use crate::model::AppendBatchInput;
use crate::model::AppendBatchResult;
use crate::model::CreateProcessInput;
use crate::model::EntrySeq;
use crate::model::InitializeProcessInput;
use crate::model::InitializeProcessResult;
use crate::model::JournalEntry;
use crate::model::Lease;
use crate::model::LoadedJournal;
use crate::model::OwnerId;
use crate::model::ProcessRecord;
use crate::store::JournalStore;
use crate::store::{
    journal_item_type, timestamp_after, timestamp_from_epoch_seconds, validate_sequences,
};

use self::codec::{deserialize_item, process_row_to_record, serialize_item};
use self::transactions::{
    ensure_process_exists, ensure_process_exists_in_tx, load_valid_lease_in_tx, next_seq_in_tx,
};

mod codec;
mod processes;
mod transactions;

pub struct SqliteJournalStore {
    pool: SqlitePool,
}

impl SqliteJournalStore {
    pub async fn open(path: &Path) -> Result<Self, JournalError> {
        let pool = open_runtime_db_at_path(path)
            .await
            .map_err(|err| JournalError::Io(std::io::Error::other(err)))?;
        Ok(Self { pool })
    }

    pub async fn default() -> Result<Self, JournalError> {
        let dir = chaos_pwd::find_chaos_home()?;
        tokio::fs::create_dir_all(&dir).await?;
        let path = runtime_db_path(dir.as_path());
        Self::open(&path).await
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

impl JournalStore for SqliteJournalStore {
    async fn archive_idle_process(
        &self,
        request: crate::ArchiveIdleProcessRequest,
    ) -> Result<(), JournalError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT p.id, p.parent_process_id, p.fork_at_seq, p.source_json,
                COALESCE((SELECT json_extract(j.payload_json, '$.payload.cwd')
                    FROM journal_entries j
                    WHERE j.process_id = p.id AND j.item_type = 'turn_context'
                    ORDER BY j.seq DESC LIMIT 1), p.cwd) AS cwd,
                p.created_at, p.updated_at, p.archived_at, p.title, p.model_provider,
                p.cli_version, p.agent_nickname, p.agent_role
             FROM processes p WHERE p.id = ?",
        )
        .bind(request.process_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else { return Ok(()) };
        let record = process_row_to_record(&row)?;
        if record.source != request.source
            || record.cwd != request.cwd
            || record.model_provider != request.model_provider
        {
            return Err(JournalError::InvalidRequest(
                "archive scope mismatch".into(),
            ));
        }
        let now = jiff::Timestamp::now();
        if let Some(row) = sqlx::query(
            "SELECT owner_id, expires_at FROM process_leases
             WHERE process_id = ? AND expires_at > ?",
        )
        .bind(request.process_id.to_string())
        .bind(now.as_second())
        .fetch_optional(&mut *tx)
        .await?
        {
            return Err(JournalError::LeaseConflict {
                process_id: request.process_id,
                current_owner_id: row.get("owner_id"),
                expires_at: timestamp_from_epoch_seconds(row.get("expires_at"))?,
            });
        }
        sqlx::query("UPDATE processes SET archived_at = COALESCE(archived_at, ?) WHERE id = ?")
            .bind(now.as_second())
            .bind(request.process_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn execution(
        &self,
        request: crate::ExecutionRequest,
    ) -> Result<crate::ExecutionResponse, JournalError> {
        self.execute_ledger(request).await
    }

    async fn create_process(
        &self,
        input: CreateProcessInput,
    ) -> Result<ProcessRecord, JournalError> {
        self.create_process_inner(input).await
    }

    async fn initialize_process(
        &self,
        input: InitializeProcessInput,
    ) -> Result<InitializeProcessResult, JournalError> {
        self.initialize_process_inner(input).await
    }

    async fn get_process(
        &self,
        process_id: &ProcessId,
    ) -> Result<Option<ProcessRecord>, JournalError> {
        self.get_process_inner(process_id).await
    }

    async fn list_processes(
        &self,
        archived: Option<bool>,
    ) -> Result<Vec<ProcessRecord>, JournalError> {
        self.list_processes_inner(archived).await
    }

    async fn acquire_lease(
        &self,
        process_id: &ProcessId,
        owner_id: &OwnerId,
        ttl: Duration,
    ) -> Result<Lease, JournalError> {
        ensure_process_exists(&self.pool, process_id).await?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let now = jiff::Timestamp::now();
        let expires_at = timestamp_after(now, ttl)?;
        let lease_token = Uuid::now_v7().to_string();

        let existing = sqlx::query(
            "SELECT owner_id, lease_token, expires_at
             FROM process_leases
             WHERE process_id = ?",
        )
        .bind(process_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;

        let existing = match existing {
            Some(row) => Some((
                row.get("owner_id"),
                timestamp_from_epoch_seconds(row.get("expires_at"))?,
            )),
            None => None,
        };
        ensure_acquirable(process_id, owner_id, existing, now)?;

        sqlx::query(
            "INSERT INTO process_leases (process_id, owner_id, lease_token, expires_at, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(process_id) DO UPDATE SET
                 owner_id = excluded.owner_id,
                 lease_token = excluded.lease_token,
                 expires_at = excluded.expires_at,
                 updated_at = excluded.updated_at",
        )
        .bind(process_id.to_string())
        .bind(owner_id)
        .bind(&lease_token)
        .bind(expires_at.as_second())
        .bind(now.as_second())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(Lease {
            process_id: *process_id,
            owner_id: owner_id.clone(),
            lease_token,
            expires_at,
        })
    }

    async fn heartbeat_lease(
        &self,
        process_id: &ProcessId,
        owner_id: &OwnerId,
        lease_token: &str,
        ttl: Duration,
    ) -> Result<Lease, JournalError> {
        // Validate and extend under one write lock so a competing acquire
        // cannot slip in between the check and the update.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        ensure_process_exists_in_tx(&mut tx, process_id).await?;
        let now = jiff::Timestamp::now();
        let lease = load_valid_lease_in_tx(&mut tx, process_id, owner_id, lease_token, now).await?;
        let expires_at = timestamp_after(now, ttl)?;

        sqlx::query(
            "UPDATE process_leases
             SET expires_at = ?, updated_at = ?
             WHERE process_id = ?",
        )
        .bind(expires_at.as_second())
        .bind(now.as_second())
        .bind(process_id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(Lease {
            expires_at,
            ..lease
        })
    }

    async fn release_lease(
        &self,
        process_id: &ProcessId,
        owner_id: &OwnerId,
        lease_token: &str,
    ) -> Result<(), JournalError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        ensure_process_exists_in_tx(&mut tx, process_id).await?;
        let _lease = load_valid_lease_in_tx(
            &mut tx,
            process_id,
            owner_id,
            lease_token,
            jiff::Timestamp::now(),
        )
        .await?;
        sqlx::query("DELETE FROM process_leases WHERE process_id = ?")
            .bind(process_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn append_batch(
        &self,
        input: AppendBatchInput,
    ) -> Result<AppendBatchResult, JournalError> {
        validate_sequences(input.process_id, input.expected_next_seq, &input.items)?;
        // Fence validation and append under one write reservation. A deferred
        // read-to-write upgrade can fail with SQLITE_BUSY despite busy_timeout,
        // and an out-of-transaction lease check can race archival or takeover.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        ensure_process_exists_in_tx(&mut tx, &input.process_id).await?;
        let now = jiff::Timestamp::now();
        let _lease = load_valid_lease_in_tx(
            &mut tx,
            &input.process_id,
            &input.owner_id,
            &input.lease_token,
            now,
        )
        .await?;

        let actual_next_seq = next_seq_in_tx(&mut tx, &input.process_id).await?;
        if actual_next_seq != input.expected_next_seq {
            return Err(JournalError::SequenceConflict {
                process_id: input.process_id,
                expected_next_seq: input.expected_next_seq,
                actual_next_seq,
            });
        }

        let mut updated_at = now;
        for entry in &input.items {
            let recorded_at = entry.recorded_at.as_second();
            let item_type = journal_item_type(&entry.item);
            let payload_json = serialize_item(&entry.item)?;
            sqlx::query(
                "INSERT INTO journal_entries (
                    process_id,
                    seq,
                    recorded_at,
                    item_type,
                    payload_json
                 ) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(input.process_id.to_string())
            .bind(entry.seq)
            .bind(recorded_at)
            .bind(item_type)
            .bind(payload_json)
            .execute(&mut *tx)
            .await?;
            updated_at = entry.recorded_at;
        }

        sqlx::query("UPDATE processes SET updated_at = ? WHERE id = ?")
            .bind(updated_at.as_second())
            .bind(input.process_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        let next_seq = input
            .items
            .last()
            .map(|entry| entry.seq + 1)
            .unwrap_or(input.expected_next_seq);

        Ok(AppendBatchResult {
            next_seq,
            updated_at,
        })
    }

    async fn load_journal(&self, process_id: &ProcessId) -> Result<LoadedJournal, JournalError> {
        let process = self
            .get_process(process_id)
            .await?
            .ok_or(JournalError::ProcessNotFound(*process_id))?;

        let rows = sqlx::query(
            "SELECT seq, recorded_at, payload_json
             FROM journal_entries
             WHERE process_id = ?
             ORDER BY seq ASC",
        )
        .bind(process_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let seq: EntrySeq = row.get("seq");
            let recorded_at = timestamp_from_epoch_seconds(row.get("recorded_at"))?;
            let item = deserialize_item(row.get("payload_json"))?;
            items.push(JournalEntry {
                seq,
                recorded_at,
                item,
            });
        }
        let next_seq = items.last().map(|item| item.seq + 1).unwrap_or(0);

        Ok(LoadedJournal {
            process_id: *process_id,
            parent: process.parent,
            items,
            next_seq,
        })
    }

    async fn get_default_process(&self) -> Result<Option<ProcessId>, JournalError> {
        let row = sqlx::query("SELECT value FROM settings WHERE key = 'default_session_id'")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            None => Ok(None),
            Some(row) => {
                let value: String = row.get("value");
                let process_id = ProcessId::from_string(&value)
                    .map_err(|source| JournalError::InvalidProcessId { value, source })?;
                Ok(Some(process_id))
            }
        }
    }

    async fn set_default_process(&self, process_id: &ProcessId) -> Result<(), JournalError> {
        sqlx::query(
            "INSERT INTO settings (key, value)
             VALUES ('default_session_id', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(process_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
