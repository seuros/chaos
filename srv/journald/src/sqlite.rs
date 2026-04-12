use std::path::Path;
use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use chaos_proc::open_runtime_db_at_path;
use chaos_proc::runtime_db_path;
use serde_json::Value;
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::JournalError;
use crate::model::AppendBatchInput;
use crate::model::AppendBatchResult;
use crate::model::CreateProcessInput;
use crate::model::EntrySeq;
use crate::model::JournalEntry;
use crate::model::Lease;
use crate::model::LoadedJournal;
use crate::model::OwnerId;
use crate::model::ParentRef;
use crate::model::ProcessRecord;
use crate::store::JournalStore;

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
    async fn create_process(
        &self,
        input: CreateProcessInput,
    ) -> Result<ProcessRecord, JournalError> {
        let source = source_label(&input.source);
        let source_json = serialize_source(&input.source)?;
        let title = input.title.clone().unwrap_or_default();
        let model_provider = input
            .model_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let cli_version = input.cli_version.clone().unwrap_or_default();
        let agent_nickname = input.source.get_nickname();
        let agent_role = input.source.get_agent_role();
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
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?)",
        )
        .bind(input.process_id.to_string())
        .bind(parent_process_id)
        .bind(fork_at_seq)
        .bind(source)
        .bind(source_json)
        .bind(input.cwd.to_string_lossy().to_string())
        .bind(created_at)
        .bind(created_at)
        .bind(&title)
        .bind(&model_provider)
        .bind(&cli_version)
        .bind(&agent_nickname)
        .bind(&agent_role)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => Ok(ProcessRecord {
                process_id: input.process_id,
                parent: input.parent,
                source: input.source,
                cwd: input.cwd,
                created_at: input.created_at,
                updated_at: input.created_at,
                archived_at: None,
                title,
                model_provider,
                cli_version: input.cli_version,
                agent_nickname,
                agent_role,
            }),
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                Err(JournalError::ProcessAlreadyExists(input.process_id))
            }
            Err(err) => Err(JournalError::Db(err)),
        }
    }

    async fn get_process(
        &self,
        process_id: &ProcessId,
    ) -> Result<Option<ProcessRecord>, JournalError> {
        let row = sqlx::query(
            "SELECT
                id,
                parent_process_id,
                fork_at_seq,
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
             FROM processes
             WHERE id = ?",
        )
        .bind(process_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        row.as_ref().map(process_row_to_record).transpose()
    }

    async fn list_processes(
        &self,
        archived: Option<bool>,
    ) -> Result<Vec<ProcessRecord>, JournalError> {
        let rows = match archived {
            Some(true) => {
                sqlx::query(
                    "SELECT
                        id,
                        parent_process_id,
                        fork_at_seq,
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
                     FROM processes
                     WHERE archived_at IS NOT NULL",
                )
                .fetch_all(&self.pool)
                .await?
            }
            Some(false) => {
                sqlx::query(
                    "SELECT
                        id,
                        parent_process_id,
                        fork_at_seq,
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
                     FROM processes
                     WHERE archived_at IS NULL",
                )
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT
                        id,
                        parent_process_id,
                        fork_at_seq,
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
                     FROM processes",
                )
                .fetch_all(&self.pool)
                .await?
            }
        };

        rows.iter().map(process_row_to_record).collect()
    }

    async fn acquire_lease(
        &self,
        process_id: &ProcessId,
        owner_id: &OwnerId,
        ttl: Duration,
    ) -> Result<Lease, JournalError> {
        ensure_process_exists(&self.pool, process_id).await?;
        let now = jiff::Timestamp::now();
        let expires_at = timestamp_after(now, ttl)?;
        let lease_token = Uuid::now_v7().to_string();

        let existing = sqlx::query(
            "SELECT owner_id, lease_token, expires_at
             FROM process_leases
             WHERE process_id = ?",
        )
        .bind(process_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = existing.as_ref() {
            let existing_owner_id: String = row.get("owner_id");
            let existing_expires_at = parse_timestamp_text(row.get("expires_at"))?;
            if existing_expires_at > now && existing_owner_id != *owner_id {
                return Err(JournalError::LeaseConflict {
                    process_id: *process_id,
                    current_owner_id: existing_owner_id,
                    expires_at: existing_expires_at,
                });
            }
        }

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
        .bind(timestamp_text(expires_at))
        .bind(now.as_second())
        .execute(&self.pool)
        .await?;

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
        ensure_process_exists(&self.pool, process_id).await?;
        let now = jiff::Timestamp::now();
        let lease = load_valid_lease(&self.pool, process_id, owner_id, lease_token, now).await?;
        let expires_at = timestamp_after(now, ttl)?;

        sqlx::query(
            "UPDATE process_leases
             SET expires_at = ?, updated_at = ?
             WHERE process_id = ?",
        )
        .bind(timestamp_text(expires_at))
        .bind(now.as_second())
        .bind(process_id.to_string())
        .execute(&self.pool)
        .await?;

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
        ensure_process_exists(&self.pool, process_id).await?;
        let _lease = load_valid_lease(
            &self.pool,
            process_id,
            owner_id,
            lease_token,
            jiff::Timestamp::now(),
        )
        .await?;
        sqlx::query("DELETE FROM process_leases WHERE process_id = ?")
            .bind(process_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn append_batch(
        &self,
        input: AppendBatchInput,
    ) -> Result<AppendBatchResult, JournalError> {
        ensure_process_exists(&self.pool, &input.process_id).await?;
        let now = jiff::Timestamp::now();
        let _lease = load_valid_lease(
            &self.pool,
            &input.process_id,
            &input.owner_id,
            &input.lease_token,
            now,
        )
        .await?;

        validate_batch_sequences(&input)?;

        let mut tx = self.pool.begin().await?;
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
            let recorded_at = timestamp_text(entry.recorded_at);
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
            let recorded_at = parse_timestamp_text(row.get("recorded_at"))?;
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

fn serialize_source(source: &SessionSource) -> Result<String, JournalError> {
    serde_json::to_string(source).map_err(|source_err| JournalError::Serialize {
        field: "source_json",
        source: source_err,
    })
}

fn source_label(source: &SessionSource) -> String {
    match serde_json::to_value(source) {
        Ok(Value::String(label)) => label,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

fn serialize_item(item: &RolloutItem) -> Result<String, JournalError> {
    serde_json::to_string(item).map_err(|source| JournalError::Serialize {
        field: "payload_json",
        source,
    })
}

fn deserialize_item(value: String) -> Result<RolloutItem, JournalError> {
    serde_json::from_str(&value).map_err(|source| JournalError::Deserialize {
        field: "payload_json",
        source,
    })
}

fn deserialize_source(value: String) -> Result<SessionSource, JournalError> {
    serde_json::from_str(&value).map_err(|source| JournalError::Deserialize {
        field: "source_json",
        source,
    })
}

fn process_row_to_record(row: &sqlx::sqlite::SqliteRow) -> Result<ProcessRecord, JournalError> {
    let process_id_text: String = row.get("id");
    let process_id = ProcessId::from_string(&process_id_text).map_err(|source| {
        JournalError::InvalidProcessId {
            value: process_id_text,
            source,
        }
    })?;
    let parent = match row.get::<Option<String>, _>("parent_process_id") {
        Some(parent_process_id_text) => {
            let parent_process_id =
                ProcessId::from_string(&parent_process_id_text).map_err(|source| {
                    JournalError::InvalidProcessId {
                        value: parent_process_id_text,
                        source,
                    }
                })?;
            Some(ParentRef {
                parent_process_id,
                fork_at_seq: row.get("fork_at_seq"),
            })
        }
        None => None,
    };
    let source = deserialize_source(row.get("source_json"))?;
    let created_at = timestamp_from_epoch_seconds(row.get("created_at"))?;
    let updated_at = timestamp_from_epoch_seconds(row.get("updated_at"))?;
    let archived_at = row
        .get::<Option<i64>, _>("archived_at")
        .map(timestamp_from_epoch_seconds)
        .transpose()?;
    let cli_version: String = row.get("cli_version");
    Ok(ProcessRecord {
        process_id,
        parent,
        source,
        cwd: row.get::<String, _>("cwd").into(),
        created_at,
        updated_at,
        archived_at,
        title: row.get("title"),
        model_provider: row.get("model_provider"),
        cli_version: (!cli_version.is_empty()).then_some(cli_version),
        agent_nickname: row.get("agent_nickname"),
        agent_role: row.get("agent_role"),
    })
}

async fn ensure_process_exists(
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

async fn load_valid_lease(
    pool: &SqlitePool,
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
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Err(JournalError::InvalidLease {
            process_id: *process_id,
        });
    };

    let stored_owner_id: String = row.get("owner_id");
    let stored_lease_token: String = row.get("lease_token");
    let expires_at = parse_timestamp_text(row.get("expires_at"))?;

    if expires_at <= now {
        return Err(JournalError::LeaseExpired {
            process_id: *process_id,
        });
    }

    if stored_owner_id != *owner_id || stored_lease_token != lease_token {
        return Err(JournalError::InvalidLease {
            process_id: *process_id,
        });
    }

    Ok(Lease {
        process_id: *process_id,
        owner_id: stored_owner_id,
        lease_token: stored_lease_token,
        expires_at,
    })
}

fn validate_batch_sequences(input: &AppendBatchInput) -> Result<(), JournalError> {
    for (index, entry) in input.items.iter().enumerate() {
        let expected = input.expected_next_seq + index as i64;
        if entry.seq != expected {
            return Err(JournalError::SequenceConflict {
                process_id: input.process_id,
                expected_next_seq: expected,
                actual_next_seq: entry.seq,
            });
        }
    }
    Ok(())
}

async fn next_seq_in_tx(
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

fn journal_item_type(item: &RolloutItem) -> &'static str {
    match item {
        RolloutItem::SessionMeta(_) => "session_meta",
        RolloutItem::ResponseItem(_) => "response_item",
        RolloutItem::Compacted(_) => "compacted",
        RolloutItem::TurnContext(_) => "turn_context",
        RolloutItem::EventMsg(_) => "event_msg",
    }
}

fn timestamp_after(base: jiff::Timestamp, ttl: Duration) -> Result<jiff::Timestamp, JournalError> {
    let ttl_seconds = i64::try_from(ttl.as_secs()).map_err(|_| JournalError::InvalidTimestamp {
        value: ttl.as_secs().to_string(),
        message: "ttl seconds overflow i64".to_string(),
    })?;
    let base_seconds = base.as_second();
    let expires =
        base_seconds
            .checked_add(ttl_seconds)
            .ok_or_else(|| JournalError::InvalidTimestamp {
                value: ttl.as_secs().to_string(),
                message: "timestamp overflow".to_string(),
            })?;
    timestamp_from_epoch_seconds(expires)
}

fn timestamp_from_epoch_seconds(seconds: i64) -> Result<jiff::Timestamp, JournalError> {
    jiff::Timestamp::from_second(seconds).map_err(|err| JournalError::InvalidTimestamp {
        value: seconds.to_string(),
        message: err.to_string(),
    })
}

fn timestamp_text(timestamp: jiff::Timestamp) -> String {
    timestamp.to_string()
}

fn parse_timestamp_text(value: String) -> Result<jiff::Timestamp, JournalError> {
    value
        .parse::<jiff::Timestamp>()
        .map_err(|err| JournalError::InvalidTimestamp {
            value,
            message: err.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chaos_ipc::ProcessId;
    use chaos_ipc::protocol::CompactedItem;
    use chaos_ipc::protocol::RolloutItem;
    use chaos_ipc::protocol::SessionSource;
    use tempfile::tempdir;

    use super::SqliteJournalStore;
    use crate::model::AppendBatchInput;
    use crate::model::CreateProcessInput;
    use crate::model::JournalEntry;
    use crate::store::JournalStore;

    #[tokio::test]
    async fn create_process_and_round_trip_journal() {
        let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let db_path = temp_dir.path().join("journal.sqlite");
        let store = SqliteJournalStore::open(&db_path)
            .await
            .unwrap_or_else(|err| panic!("open: {err}"));

        let process_id = ProcessId::new();
        let created_at = jiff::Timestamp::now();
        let process = store
            .create_process(CreateProcessInput {
                process_id,
                parent: None,
                source: SessionSource::Cli,
                cwd: temp_dir.path().to_path_buf(),
                created_at,
                title: Some("test journal".to_string()),
                model_provider: Some("openai".to_string()),
                cli_version: Some("47.0.0".to_string()),
            })
            .await
            .unwrap_or_else(|err| panic!("create_process: {err}"));

        assert_eq!(process.process_id, process_id);
        assert_eq!(process.title, "test journal");

        let lease = store
            .acquire_lease(&process_id, &"owner-1".to_string(), Duration::from_secs(30))
            .await
            .unwrap_or_else(|err| panic!("acquire_lease: {err}"));

        let first_item = RolloutItem::Compacted(CompactedItem {
            message: "hello".to_string(),
            replacement_history: None,
        });
        let append = store
            .append_batch(AppendBatchInput {
                process_id,
                owner_id: "owner-1".to_string(),
                lease_token: lease.lease_token.clone(),
                expected_next_seq: 0,
                items: vec![JournalEntry {
                    seq: 0,
                    recorded_at: jiff::Timestamp::now(),
                    item: first_item.clone(),
                }],
            })
            .await
            .unwrap_or_else(|err| panic!("append_batch: {err}"));

        assert_eq!(append.next_seq, 1);

        let loaded = store
            .load_journal(&process_id)
            .await
            .unwrap_or_else(|err| panic!("load_journal: {err}"));
        assert_eq!(loaded.process_id, process_id);
        assert_eq!(loaded.next_seq, 1);
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].seq, 0);
        let loaded_item_json = serde_json::to_string(&loaded.items[0].item)
            .unwrap_or_else(|err| panic!("serialize loaded item: {err}"));
        let first_item_json = serde_json::to_string(&first_item)
            .unwrap_or_else(|err| panic!("serialize item: {err}"));
        assert_eq!(loaded_item_json, first_item_json);
    }

    #[tokio::test]
    async fn rejects_append_with_wrong_expected_next_seq() {
        let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let db_path = temp_dir.path().join("journal.sqlite");
        let store = SqliteJournalStore::open(&db_path)
            .await
            .unwrap_or_else(|err| panic!("open: {err}"));
        let process_id = ProcessId::new();
        store
            .create_process(CreateProcessInput {
                process_id,
                parent: None,
                source: SessionSource::Cli,
                cwd: temp_dir.path().to_path_buf(),
                created_at: jiff::Timestamp::now(),
                title: None,
                model_provider: None,
                cli_version: None,
            })
            .await
            .unwrap_or_else(|err| panic!("create_process: {err}"));
        let lease = store
            .acquire_lease(&process_id, &"owner-1".to_string(), Duration::from_secs(30))
            .await
            .unwrap_or_else(|err| panic!("acquire_lease: {err}"));

        let err = match store
            .append_batch(AppendBatchInput {
                process_id,
                owner_id: "owner-1".to_string(),
                lease_token: lease.lease_token,
                expected_next_seq: 5,
                items: vec![JournalEntry {
                    seq: 5,
                    recorded_at: jiff::Timestamp::now(),
                    item: RolloutItem::Compacted(CompactedItem {
                        message: "bad seq".to_string(),
                        replacement_history: None,
                    }),
                }],
            })
            .await
        {
            Ok(_) => panic!("append_batch unexpectedly succeeded"),
            Err(err) => err,
        };

        match err {
            crate::error::JournalError::SequenceConflict { .. } => {}
            other => panic!("unexpected error: {other}"),
        }
    }
}
