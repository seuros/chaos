use crate::AgentJob;
use crate::AgentJobCreateParams;
use crate::AgentJobItem;
use crate::AgentJobItemCreateParams;
use crate::AgentJobItemStatus;
use crate::AgentJobProgress;
use crate::AgentJobStatus;
use crate::CHAOS_DB_FILENAME;
use crate::CHAOS_DB_VERSION;
use crate::LOGS_DB_FILENAME;
use crate::LOGS_DB_VERSION;
use crate::LogEntry;
use crate::LogQuery;
use crate::LogRow;
use crate::ProcessMetadata;
use crate::ProcessMetadataBuilder;
use crate::ProcessesPage;
use crate::STATE_DB_FILENAME;
use crate::STATE_DB_VERSION;
use crate::SortKey;
use crate::apply_rollout_item;
use crate::migrations::CHAOS_MIGRATOR;
use crate::migrations::LOGS_MIGRATOR;
use crate::migrations::STATE_MIGRATOR;
use crate::model::AgentJobRow;
use crate::model::ProcessRow;
use crate::model::anchor_from_item;
use crate::model::datetime_to_epoch_seconds;
use chaos_ipc::ProcessId;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::protocol::RolloutItem;
use log::LevelFilter;
use serde_json::Value;
use sqlx::ConnectOptions;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqliteConnection;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::sqlite::SqliteSynchronous;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

mod agent_jobs;
mod backfill;
mod logs;
mod memories;
mod processes;
#[cfg(test)]
mod test_support;

// "Partition" is the retention bucket we cap at 10 MiB:
// - one bucket per non-null process_id
// - one bucket per processless (process_id IS NULL) non-null process_uuid
// - one bucket for processless rows with process_uuid IS NULL
const LOG_PARTITION_SIZE_LIMIT_BYTES: i64 = 10 * 1024 * 1024;
const LOG_PARTITION_ROW_LIMIT: i64 = 1_000;

#[derive(Clone)]
pub struct StateRuntime {
    codex_home: PathBuf,
    default_provider: String,
    pool: Arc<sqlx::SqlitePool>,
    logs_pool: Arc<sqlx::SqlitePool>,
    chaos_pool: Option<Arc<sqlx::SqlitePool>>,
}

impl StateRuntime {
    /// Initialize the state runtime using the provided Codex home and default provider.
    ///
    /// This opens (and migrates) the SQLite databases under `codex_home`,
    /// keeping logs in a dedicated file to reduce lock contention with the
    /// rest of the state store.
    pub async fn init(codex_home: PathBuf, default_provider: String) -> anyhow::Result<Arc<Self>> {
        tokio::fs::create_dir_all(&codex_home).await?;
        let current_state_name = state_db_filename();
        let current_logs_name = logs_db_filename();
        remove_legacy_db_files(
            &codex_home,
            current_state_name.as_str(),
            STATE_DB_FILENAME,
            "state",
        )
        .await;
        remove_legacy_db_files(
            &codex_home,
            current_logs_name.as_str(),
            LOGS_DB_FILENAME,
            "logs",
        )
        .await;
        let state_path = state_db_path(codex_home.as_path());
        let logs_path = logs_db_path(codex_home.as_path());
        let pool = match open_sqlite(&state_path, &STATE_MIGRATOR).await {
            Ok(db) => Arc::new(db),
            Err(err) => {
                warn!("failed to open state db at {}: {err}", state_path.display());
                return Err(err);
            }
        };
        let logs_pool = match open_sqlite(&logs_path, &LOGS_MIGRATOR).await {
            Ok(db) => Arc::new(db),
            Err(err) => {
                warn!("failed to open logs db at {}: {err}", logs_path.display());
                return Err(err);
            }
        };
        let chaos_path = chaos_db_path(codex_home.as_path());
        let chaos_pool = match open_chaos_db(codex_home.as_path()).await {
            Ok(db) => Some(Arc::new(db)),
            Err(err) => {
                warn!(
                    "failed to open chaos db at {}: {err} — cron and other chaos-native features will be unavailable",
                    chaos_path.display()
                );
                None
            }
        };
        let runtime = Arc::new(Self {
            pool,
            logs_pool,
            chaos_pool,
            codex_home,
            default_provider,
        });
        Ok(runtime)
    }

    /// Return the configured Codex home directory for this runtime.
    pub fn codex_home(&self) -> &Path {
        self.codex_home.as_path()
    }

    /// Return a reference to the Chaos-native SQLite pool, if available.
    ///
    /// Returns `None` when the chaos DB failed to open at init time.
    /// Callers should degrade gracefully rather than treating this as fatal.
    pub fn chaos_pool(&self) -> Option<&SqlitePool> {
        self.chaos_pool.as_deref()
    }
}

async fn open_sqlite(path: &Path, migrator: &'static Migrator) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5))
        .log_statements(LevelFilter::Off);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    migrator.run(&pool).await?;
    Ok(pool)
}

fn db_filename(base_name: &str, version: u32) -> String {
    format!("{base_name}_{version}.sqlite")
}

pub fn state_db_filename() -> String {
    db_filename(STATE_DB_FILENAME, STATE_DB_VERSION)
}

pub fn state_db_path(codex_home: &Path) -> PathBuf {
    codex_home.join(state_db_filename())
}

pub fn logs_db_filename() -> String {
    db_filename(LOGS_DB_FILENAME, LOGS_DB_VERSION)
}

pub fn logs_db_path(codex_home: &Path) -> PathBuf {
    codex_home.join(logs_db_filename())
}

pub fn chaos_db_filename() -> String {
    db_filename(CHAOS_DB_FILENAME, CHAOS_DB_VERSION)
}

pub fn chaos_db_path(codex_home: &Path) -> PathBuf {
    codex_home.join(chaos_db_filename())
}

pub async fn open_chaos_db(codex_home: &Path) -> anyhow::Result<SqlitePool> {
    open_sqlite(&chaos_db_path(codex_home), &CHAOS_MIGRATOR).await
}

async fn remove_legacy_db_files(
    codex_home: &Path,
    current_name: &str,
    base_name: &str,
    db_label: &str,
) {
    let mut entries = match tokio::fs::read_dir(codex_home).await {
        Ok(entries) => entries,
        Err(err) => {
            warn!(
                "failed to read codex_home for {db_label} db cleanup {}: {err}",
                codex_home.display(),
            );
            return;
        }
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !entry
            .file_type()
            .await
            .map(|file_type| file_type.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if !should_remove_db_file(file_name.as_ref(), current_name, base_name) {
            continue;
        }

        let legacy_path = entry.path();
        if let Err(err) = tokio::fs::remove_file(&legacy_path).await {
            warn!(
                "failed to remove legacy {db_label} db file {}: {err}",
                legacy_path.display(),
            );
        }
    }
}

fn should_remove_db_file(file_name: &str, current_name: &str, base_name: &str) -> bool {
    let mut normalized_name = file_name;
    for suffix in ["-wal", "-shm", "-journal"] {
        if let Some(stripped) = file_name.strip_suffix(suffix) {
            normalized_name = stripped;
            break;
        }
    }
    if normalized_name == current_name {
        return false;
    }
    let unversioned_name = format!("{base_name}.sqlite");
    if normalized_name == unversioned_name {
        return true;
    }

    let Some(version_with_extension) = normalized_name.strip_prefix(&format!("{base_name}_"))
    else {
        return false;
    };
    let Some(version_suffix) = version_with_extension.strip_suffix(".sqlite") else {
        return false;
    };
    !version_suffix.is_empty() && version_suffix.chars().all(|ch| ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    #[tokio::test]
    async fn init_survives_chaos_db_open_failure() {
        let codex_home = test_support::unique_temp_dir();
        tokio::fs::create_dir_all(&codex_home)
            .await
            .expect("create temp codex home");

        let blocking_chaos_path = codex_home.join(chaos_db_filename());
        tokio::fs::create_dir_all(&blocking_chaos_path)
            .await
            .expect("create blocking chaos path");

        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("state runtime should still initialize");

        assert!(
            runtime.chaos_pool().is_none(),
            "chaos db should be unavailable when its path is not openable as sqlite"
        );
        assert!(
            runtime.get_backfill_state().await.is_ok(),
            "state db should remain usable when only chaos db init fails"
        );

        tokio::fs::remove_dir_all(&codex_home)
            .await
            .expect("cleanup temp codex home");
    }

    #[tokio::test]
    async fn open_chaos_db_creates_schema() {
        let codex_home = test_support::unique_temp_dir();
        tokio::fs::create_dir_all(&codex_home)
            .await
            .expect("create temp codex home");

        let pool = open_chaos_db(codex_home.as_path())
            .await
            .expect("open chaos db");

        let row = sqlx::query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'cron_jobs'",
        )
        .fetch_one(&pool)
        .await
        .expect("cron_jobs table should exist");

        let table_name: String = row.get("name");
        assert_eq!(table_name, "cron_jobs");
        assert!(
            tokio::fs::try_exists(&chaos_db_path(codex_home.as_path()))
                .await
                .expect("stat chaos db"),
            "chaos db file should be created on demand"
        );

        tokio::fs::remove_dir_all(&codex_home)
            .await
            .expect("cleanup temp codex home");
    }
}
