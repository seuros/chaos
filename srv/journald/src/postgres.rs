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
        for (index, entry) in input.items.iter().enumerate() {
            let expected = index as i64;
            if entry.seq != expected {
                return Err(JournalError::SequenceConflict {
                    process_id: input.create.process_id,
                    expected_next_seq: expected,
                    actual_next_seq: entry.seq,
                });
            }
        }

        let create = &input.create;
        let source = source_label(&create.source);
        let source_json = serialize_source(&create.source)?;
        let title = create.title.clone().unwrap_or_default();
        let model_provider = create
            .model_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let cli_version = create.cli_version.clone().unwrap_or_default();
        let agent_nickname = create.source.get_nickname();
        let agent_role = create.source.get_agent_role();
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
        .bind(&title)
        .bind(&model_provider)
        .bind(&cli_version)
        .bind(&agent_nickname)
        .bind(&agent_role)
        .execute(&mut *tx)
        .await;
        if let Err(err) = insert_process {
            return match err {
                sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
                    Err(JournalError::ProcessAlreadyExists(create.process_id))
                }
                other => Err(JournalError::Db(other)),
            };
        }

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
            process: ProcessRecord {
                process_id: create.process_id,
                parent: create.parent.clone(),
                source: create.source.clone(),
                cwd: create.cwd.clone(),
                created_at: create.created_at,
                updated_at,
                archived_at: None,
                title,
                model_provider,
                cli_version: create.cli_version.clone(),
                agent_nickname,
                agent_role,
            },
            lease: Lease {
                process_id: create.process_id,
                owner_id: input.owner_id,
                lease_token,
                expires_at: lease_expires_at,
            },
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
        let now = jiff::Timestamp::now();
        let expires_at = timestamp_after(now, ttl)?;
        let lease_token = Uuid::now_v7().to_string();
        let mut tx = self.pool.begin().await?;
        lock_process(&mut tx, process_id).await?;

        let existing = sqlx::query(
            "SELECT owner_id, lease_token, expires_at
             FROM process_leases
             WHERE process_id = $1
             FOR UPDATE",
        )
        .bind(process_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = existing.as_ref() {
            let existing_owner_id: String = row.get("owner_id");
            let existing_expires_at = timestamp_from_epoch_seconds(row.get("expires_at"))?;
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
        let now = jiff::Timestamp::now();
        let expires_at = timestamp_after(now, ttl)?;
        let mut tx = self.pool.begin().await?;
        lock_process(&mut tx, process_id).await?;
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
        validate_batch_sequences(&input)?;
        let now = jiff::Timestamp::now();
        let mut tx = self.pool.begin().await?;
        lock_process(&mut tx, &input.process_id).await?;
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
    let Some(row) = row else {
        return Err(JournalError::InvalidLease {
            process_id: *process_id,
        });
    };

    let stored_owner_id: String = row.get("owner_id");
    let stored_lease_token: String = row.get("lease_token");
    let expires_at = timestamp_from_epoch_seconds(row.get("expires_at"))?;
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
    serde_json::to_value(source).map_err(|source| JournalError::Serialize {
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

fn source_label(source: &SessionSource) -> String {
    match serde_json::to_value(source) {
        Ok(Value::String(label)) => label,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

fn serialize_item(item: &RolloutItem) -> Result<Value, JournalError> {
    serde_json::to_value(item).map_err(|source| JournalError::Serialize {
        field: "payload_json",
        source,
    })
}

fn deserialize_item(value: Value) -> Result<RolloutItem, JournalError> {
    serde_json::from_value(value).map_err(|source| JournalError::Deserialize {
        field: "payload_json",
        source,
    })
}

fn journal_item_type(item: &RolloutItem) -> &'static str {
    match item {
        RolloutItem::SessionMeta(_) => "session_meta",
        RolloutItem::ResponseItem(_) => "response_item",
        RolloutItem::Compacted(_) => "compacted",
        RolloutItem::CompactionControl(_) => "compaction_control",
        RolloutItem::TurnContext(_) => "turn_context",
        RolloutItem::EventMsg(_) => "event_msg",
    }
}

fn timestamp_after(base: jiff::Timestamp, ttl: Duration) -> Result<jiff::Timestamp, JournalError> {
    let ttl_seconds = i64::try_from(ttl.as_secs()).map_err(|_| JournalError::InvalidTimestamp {
        value: ttl.as_secs().to_string(),
        message: "ttl seconds overflow i64".to_string(),
    })?;
    let expires = base.as_second().checked_add(ttl_seconds).ok_or_else(|| {
        JournalError::InvalidTimestamp {
            value: ttl.as_secs().to_string(),
            message: "timestamp overflow".to_string(),
        }
    })?;
    timestamp_from_epoch_seconds(expires)
}

fn timestamp_from_epoch_seconds(seconds: i64) -> Result<jiff::Timestamp, JournalError> {
    jiff::Timestamp::from_second(seconds).map_err(|err| JournalError::InvalidTimestamp {
        value: seconds.to_string(),
        message: err.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use chaos_ipc::ProcessId;
    use chaos_ipc::protocol::CompactedItem;
    use chaos_ipc::protocol::RolloutItem;
    use chaos_ipc::protocol::SessionSource;

    use crate::AppendBatchInput;
    use crate::CreateProcessInput;
    use crate::InitializeProcessInput;
    use crate::JournalClient;
    use crate::JournalEntry;

    const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

    fn postgres_test_url() -> Option<String> {
        std::env::var(TEST_DATABASE_URL_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn compacted(message: &str) -> RolloutItem {
        RolloutItem::Compacted(CompactedItem {
            message: message.to_string(),
            replacement_history: None,
        })
    }

    #[tokio::test]
    async fn postgres_client_supports_complete_journal_lifecycle() {
        let Some(database_url) = postgres_test_url() else {
            eprintln!("skipping PostgreSQL journal validation; {TEST_DATABASE_URL_ENV} is not set");
            return;
        };
        let pool = chaos_proc::open_runtime_db_postgres_url(&database_url)
            .await
            .expect("open PostgreSQL runtime database");
        let client = JournalClient::postgres_pool(pool.clone());
        assert!(matches!(&client, JournalClient::Postgres(_)));

        let process_id = ProcessId::new();
        let first_item = compacted("first");
        let initialized = client
            .initialize_process(InitializeProcessInput {
                create: CreateProcessInput {
                    process_id,
                    parent: None,
                    source: SessionSource::Cli,
                    cwd: std::env::temp_dir().join(process_id.to_string()),
                    created_at: jiff::Timestamp::now(),
                    title: Some("PostgreSQL journal lifecycle".to_string()),
                    model_provider: Some("openai".to_string()),
                    cli_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                },
                owner_id: "postgres-owner-initial".to_string(),
                ttl_ms: 30_000,
                items: vec![JournalEntry {
                    seq: 0,
                    recorded_at: jiff::Timestamp::now(),
                    item: first_item.clone(),
                }],
            })
            .await
            .expect("initialize PostgreSQL journal");
        assert_eq!(initialized.next_seq, 1);

        let loaded = client
            .load_journal(process_id)
            .await
            .expect("load initialized PostgreSQL journal");
        assert_eq!(loaded.next_seq, 1);
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(
            serde_json::to_value(&loaded.items[0].item).expect("serialize loaded item"),
            serde_json::to_value(&first_item).expect("serialize first item")
        );

        client
            .release_lease(
                process_id,
                initialized.lease.owner_id,
                initialized.lease.lease_token,
            )
            .await
            .expect("release initial PostgreSQL lease");
        let resumed_owner = "postgres-owner-resumed".to_string();
        let resumed_lease = client
            .acquire_lease(process_id, resumed_owner.clone(), 30_000)
            .await
            .expect("acquire resumed PostgreSQL lease");
        let second_item = compacted("second");
        client
            .append_batch(AppendBatchInput {
                process_id,
                owner_id: resumed_owner.clone(),
                lease_token: resumed_lease.lease_token.clone(),
                expected_next_seq: loaded.next_seq,
                items: vec![JournalEntry {
                    seq: loaded.next_seq,
                    recorded_at: jiff::Timestamp::now(),
                    item: second_item.clone(),
                }],
            })
            .await
            .expect("append resumed PostgreSQL journal");

        let loaded = client
            .load_journal(process_id)
            .await
            .expect("load appended PostgreSQL journal");
        assert_eq!(loaded.next_seq, 2);
        assert_eq!(loaded.items.len(), 2);
        assert_eq!(
            serde_json::to_value(&loaded.items[1].item).expect("serialize loaded item"),
            serde_json::to_value(&second_item).expect("serialize second item")
        );

        let previous_default = client
            .get_default_process()
            .await
            .expect("read prior default process");
        client
            .set_default_process(process_id)
            .await
            .expect("set PostgreSQL default process");
        assert_eq!(
            client
                .get_default_process()
                .await
                .expect("read PostgreSQL default process"),
            Some(process_id)
        );
        match previous_default {
            Some(previous) => client
                .set_default_process(previous)
                .await
                .expect("restore prior default process"),
            None => {
                sqlx::query("DELETE FROM settings WHERE key = 'default_session_id'")
                    .execute(&pool)
                    .await
                    .expect("clear test default process");
            }
        }

        client
            .release_lease(process_id, resumed_owner, resumed_lease.lease_token)
            .await
            .expect("release resumed PostgreSQL lease");
    }
}
