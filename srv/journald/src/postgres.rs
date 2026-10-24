use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use serde_json::Value;
use sqlx::PgPool;
use sqlx::Postgres;
use sqlx::Row;
use sqlx::Transaction;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::error::JournalError;
use crate::lease_policy::{ensure_acquirable, validate_lease};
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
use crate::model::ParentRef;
use crate::model::ProcessRecord;
use crate::store::JournalStore;
use crate::store::{
    journal_item_type, new_process_record, process_insert_error, source_label, timestamp_after,
    timestamp_from_epoch_seconds, validate_sequences,
};

macro_rules! process_select_sql {
    ($predicate:literal $(,)?) => {
        concat!(
            "SELECT
                p.id,
                p.parent_process_id,
                p.fork_at_seq,
                p.source_json,
                COALESCE(
                    (
                        SELECT j.payload_json #>> '{payload,cwd}'
                        FROM journal_entries AS j
                        WHERE j.process_id = p.id
                          AND j.item_type = 'turn_context'
                        ORDER BY j.seq DESC
                        LIMIT 1
                    ),
                    p.cwd
                ) AS cwd,
                p.created_at,
                p.updated_at,
                p.archived_at,
                p.title,
                p.model_provider,
                p.cli_version,
                p.agent_nickname,
                p.agent_role
             FROM processes AS p ",
            $predicate
        )
    };
}

#[derive(Debug, Clone)]
pub struct PostgresJournalStore {
    pool: PgPool,
}

impl PostgresJournalStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl JournalStore for PostgresJournalStore {
    async fn archive_idle_process(
        &self,
        request: crate::ArchiveIdleProcessRequest,
    ) -> Result<(), JournalError> {
        let mut tx = self.pool.begin().await?;
        // The same process-row lock fences acquire/heartbeat/append.
        let row = sqlx::query(process_select_sql!("WHERE p.id = $1 FOR UPDATE"))
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
             WHERE process_id = $1 AND expires_at > $2",
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
        sqlx::query("UPDATE processes SET archived_at = COALESCE(archived_at, $1) WHERE id = $2")
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
        match request {
            crate::ExecutionRequest::Billing { request } => self
                .execute_billing(request)
                .await
                .map(crate::ExecutionResponse::Billing),
            request => self.execute_ledger(request).await,
        }
    }

    async fn create_process(
        &self,
        input: CreateProcessInput,
    ) -> Result<ProcessRecord, JournalError> {
        let created_at = input.created_at;
        let input = new_process_record(input, created_at);
        let source = source_label(&input.source);
        let source_json = serialize_source(&input.source)?;
        let created_at = input.created_at.as_second();
        let parent_process_id = input
            .parent
            .as_ref()
            .map(|parent| parent.parent_process_id.to_string());
        let fork_at_seq = input.parent.as_ref().map(|parent| parent.fork_at_seq);

        let result = sqlx::query(
            "INSERT INTO processes (
                id,
                parent_process_id,
                fork_at_seq,
                source,
                source_json,
                cwd,
                created_at,
                updated_at,
                archived_at,
                title,
                model_provider,
                cli_version,
                agent_nickname,
                agent_role
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9, $10, $11, $12, $13)",
        )
        .bind(input.process_id.to_string())
        .bind(parent_process_id)
        .bind(fork_at_seq)
        .bind(source)
        .bind(source_json)
        .bind(input.cwd.to_string_lossy().to_string())
        .bind(created_at)
        .bind(created_at)
        .bind(&input.title)
        .bind(&input.model_provider)
        .bind(input.cli_version.as_deref().unwrap_or_default())
        .bind(&input.agent_nickname)
        .bind(&input.agent_role)
        .execute(&self.pool)
        .await;

        result.map_err(|err| process_insert_error(input.process_id, err))?;
        Ok(input)
    }

    async fn initialize_process(
        &self,
        input: InitializeProcessInput,
    ) -> Result<InitializeProcessResult, JournalError> {
        if input.items.is_empty() {
            return Err(JournalError::InvalidRequest(format!(
                "initialize_process for {} requires at least one journal entry; \
                 use create_process + acquire_lease for empty-journal cases",
                input.create.process_id
            )));
        }
        validate_sequences(input.create.process_id, 0, &input.items)?;

        let create = &input.create;
        let source = source_label(&create.source);
        let source_json = serialize_source(&create.source)?;
        let parent_process_id = create
            .parent
            .as_ref()
            .map(|parent| parent.parent_process_id.to_string());
        let fork_at_seq = create.parent.as_ref().map(|parent| parent.fork_at_seq);

        let now = jiff::Timestamp::now();
        let lease_expires_at = timestamp_after(now, Duration::from_millis(input.ttl_ms))?;
        let lease_token = Uuid::now_v7().to_string();
        let updated_at = input
            .items
            .last()
            .map(|entry| entry.recorded_at)
            .unwrap_or(now);
        let next_seq = input.items.last().map(|entry| entry.seq + 1).unwrap_or(0);
        let create = new_process_record(input.create, updated_at);

        let mut tx = self.pool.begin().await?;
        let insert_process = sqlx::query(
            "INSERT INTO processes (
                id,
                parent_process_id,
                fork_at_seq,
                source,
                source_json,
                cwd,
                created_at,
                updated_at,
                archived_at,
                title,
                model_provider,
                cli_version,
                agent_nickname,
                agent_role
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9, $10, $11, $12, $13)",
        )
        .bind(create.process_id.to_string())
        .bind(parent_process_id)
        .bind(fork_at_seq)
        .bind(&source)
        .bind(&source_json)
        .bind(create.cwd.to_string_lossy().to_string())
        .bind(create.created_at.as_second())
        .bind(updated_at.as_second())
        .bind(&create.title)
        .bind(&create.model_provider)
        .bind(create.cli_version.as_deref().unwrap_or_default())
        .bind(&create.agent_nickname)
        .bind(&create.agent_role)
        .execute(&mut *tx)
        .await;
        insert_process.map_err(|err| process_insert_error(create.process_id, err))?;

        for entry in &input.items {
            insert_journal_entry(&mut tx, &create.process_id, entry).await?;
        }

        sqlx::query(
            "INSERT INTO process_leases (process_id, owner_id, lease_token, expires_at, updated_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(create.process_id.to_string())
        .bind(&input.owner_id)
        .bind(&lease_token)
        .bind(lease_expires_at.as_second())
        .bind(now.as_second())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(InitializeProcessResult {
            lease: Lease {
                process_id: create.process_id,
                owner_id: input.owner_id,
                lease_token,
                expires_at: lease_expires_at,
            },
            process: create,
            next_seq,
            updated_at,
        })
    }

    async fn get_process(
        &self,
        process_id: &ProcessId,
    ) -> Result<Option<ProcessRecord>, JournalError> {
        let row = sqlx::query(process_select_sql!("WHERE p.id = $1"))
            .bind(process_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(process_row_to_record).transpose()
    }

    async fn list_processes(
        &self,
        archived: Option<bool>,
    ) -> Result<Vec<ProcessRecord>, JournalError> {
        let rows = sqlx::query(process_select_sql!(
            "WHERE (
                $1::BOOLEAN IS NULL
                OR ($1 = TRUE AND p.archived_at IS NOT NULL)
                OR ($1 = FALSE AND p.archived_at IS NULL)
             )",
        ))
        .bind(archived)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(process_row_to_record).collect()
    }

    async fn acquire_lease(
        &self,
        process_id: &ProcessId,
        owner_id: &OwnerId,
        ttl: Duration,
    ) -> Result<Lease, JournalError> {
        let lease_token = Uuid::now_v7().to_string();
        let mut tx = self.pool.begin().await?;
        lock_process(&mut tx, process_id).await?;
        let now = jiff::Timestamp::now();
        let expires_at = timestamp_after(now, ttl)?;

        let existing = sqlx::query(
            "SELECT owner_id, lease_token, expires_at
             FROM process_leases
             WHERE process_id = $1
             FOR UPDATE",
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
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (process_id) DO UPDATE SET
                 owner_id = EXCLUDED.owner_id,
                 lease_token = EXCLUDED.lease_token,
                 expires_at = EXCLUDED.expires_at,
                 updated_at = EXCLUDED.updated_at",
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
        let mut tx = self.pool.begin().await?;
        lock_process(&mut tx, process_id).await?;
        let now = jiff::Timestamp::now();
        let expires_at = timestamp_after(now, ttl)?;
        let lease = load_valid_lease_in_tx(&mut tx, process_id, owner_id, lease_token, now).await?;

        sqlx::query(
            "UPDATE process_leases
             SET expires_at = $1, updated_at = $2
             WHERE process_id = $3",
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
        let mut tx = self.pool.begin().await?;
        lock_process(&mut tx, process_id).await?;
        load_valid_lease_in_tx(
            &mut tx,
            process_id,
            owner_id,
            lease_token,
            jiff::Timestamp::now(),
        )
        .await?;
        sqlx::query("DELETE FROM process_leases WHERE process_id = $1")
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
        let mut tx = self.pool.begin().await?;
        lock_process(&mut tx, &input.process_id).await?;
        let now = jiff::Timestamp::now();
        load_valid_lease_in_tx(
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
            insert_journal_entry(&mut tx, &input.process_id, entry).await?;
            updated_at = entry.recorded_at;
        }
        sqlx::query("UPDATE processes SET updated_at = $1 WHERE id = $2")
            .bind(updated_at.as_second())
            .bind(input.process_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        Ok(AppendBatchResult {
            next_seq: input
                .items
                .last()
                .map(|entry| entry.seq + 1)
                .unwrap_or(input.expected_next_seq),
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
             WHERE process_id = $1
             ORDER BY seq ASC",
        )
        .bind(process_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            items.push(JournalEntry {
                seq: row.get("seq"),
                recorded_at: timestamp_from_epoch_seconds(row.get("recorded_at"))?,
                item: deserialize_item(row.get("payload_json"))?,
            });
        }
        let next_seq = items.last().map(|entry| entry.seq + 1).unwrap_or(0);
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
                ProcessId::from_string(&value)
                    .map(Some)
                    .map_err(|source| JournalError::InvalidProcessId { value, source })
            }
        }
    }

    async fn set_default_process(&self, process_id: &ProcessId) -> Result<(), JournalError> {
        sqlx::query(
            "INSERT INTO settings (key, value)
             VALUES ('default_session_id', $1)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(process_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

async fn lock_process(
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

async fn load_valid_lease_in_tx(
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

async fn next_seq_in_tx(
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

async fn insert_journal_entry(
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

fn process_row_to_record(row: &PgRow) -> Result<ProcessRecord, JournalError> {
    let process_id_text: String = row.get("id");
    let process_id = parse_process_id(process_id_text)?;
    let parent = match row.get::<Option<String>, _>("parent_process_id") {
        Some(parent_process_id_text) => Some(ParentRef {
            parent_process_id: parse_process_id(parent_process_id_text)?,
            fork_at_seq: row.get("fork_at_seq"),
        }),
        None => None,
    };
    let source = deserialize_source(row.get("source_json"))?;
    let cli_version: String = row.get("cli_version");
    Ok(ProcessRecord {
        process_id,
        parent,
        source,
        cwd: row.get::<String, _>("cwd").into(),
        created_at: timestamp_from_epoch_seconds(row.get("created_at"))?,
        updated_at: timestamp_from_epoch_seconds(row.get("updated_at"))?,
        archived_at: row
            .get::<Option<i64>, _>("archived_at")
            .map(timestamp_from_epoch_seconds)
            .transpose()?,
        title: row.get("title"),
        model_provider: row.get("model_provider"),
        cli_version: (!cli_version.is_empty()).then_some(cli_version),
        agent_nickname: row.get("agent_nickname"),
        agent_role: row.get("agent_role"),
    })
}

fn parse_process_id(value: String) -> Result<ProcessId, JournalError> {
    ProcessId::from_string(&value)
        .map_err(|source| JournalError::InvalidProcessId { value, source })
}

fn serialize_source(source: &SessionSource) -> Result<Value, JournalError> {
    serde_json::to_value(source)
        .map(jsonb_safe)
        .map_err(|source| JournalError::Serialize {
            field: "source_json",
            source,
        })
}

fn deserialize_source(value: Value) -> Result<SessionSource, JournalError> {
    serde_json::from_value(value).map_err(|source| JournalError::Deserialize {
        field: "source_json",
        source,
    })
}

fn serialize_item(item: &RolloutItem) -> Result<Value, JournalError> {
    serde_json::to_value(item)
        .map(jsonb_safe)
        .map_err(|source| JournalError::Serialize {
            field: "payload_json",
            source,
        })
}

/// PostgreSQL `jsonb` cannot store U+0000; it is stored as U+FFFD instead.
fn jsonb_safe(mut value: Value) -> Value {
    replace_nul(&mut value);
    value
}

fn replace_nul(value: &mut Value) {
    match value {
        Value::String(text) => replace_nul_in(text),
        Value::Array(items) => items.iter_mut().for_each(replace_nul),
        Value::Object(map) => {
            if map.keys().any(|key| key.contains('\0')) {
                *map = std::mem::take(map)
                    .into_iter()
                    .map(|(mut key, value)| {
                        replace_nul_in(&mut key);
                        (key, value)
                    })
                    .collect();
            }
            map.values_mut().for_each(replace_nul);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn replace_nul_in(text: &mut String) {
    if text.contains('\0') {
        *text = text.replace('\0', "\u{FFFD}");
    }
}

fn deserialize_item(value: Value) -> Result<RolloutItem, JournalError> {
    serde_json::from_value(value).map_err(|source| JournalError::Deserialize {
        field: "payload_json",
        source,
    })
}

#[cfg(test)]
mod tests;
