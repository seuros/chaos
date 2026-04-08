use crate::AgentJob;
use crate::AgentJobCreateParams;
use crate::AgentJobItem;
use crate::AgentJobItemCreateParams;
use crate::AgentJobItemStatus;
use crate::AgentJobProgress;
use crate::AgentJobStatus;
use crate::LogEntry;
use crate::LogQuery;
use crate::LogRow;
use crate::ProcessMetadata;
use crate::ProcessMetadataBuilder;
use crate::ProcessesPage;
use crate::SortKey;
use crate::apply_rollout_item;
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
use sqlx::sqlite::SqliteAutoVacuum;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::sqlite::SqliteSynchronous;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
mod agent_jobs;
mod backfill;
mod logs;
mod memories;
mod message_history;
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
    chaos_home: PathBuf,
    default_provider: String,
    pool: Arc<sqlx::SqlitePool>,
}

impl StateRuntime {
    /// Initialize the runtime DB using the provided ChaOS home and default provider.
    ///
    /// This opens (and migrates) the SQLite database under `chaos_home`.
    ///
    pub async fn init(chaos_home: PathBuf, default_provider: String) -> anyhow::Result<Arc<Self>> {
        tokio::fs::create_dir_all(&chaos_home).await?;
        let runtime_path = runtime_db_path(chaos_home.as_path());
        let pool = Arc::new(open_sqlite(&runtime_path, &STATE_MIGRATOR).await?);
        let runtime = Arc::new(Self {
            pool,
            chaos_home,
            default_provider,
        });
        Ok(runtime)
    }

    /// Return the configured ChaOS home directory for this runtime.
    pub fn chaos_home(&self) -> &Path {
        self.chaos_home.as_path()
    }

    /// Return a reference to the runtime SQLite pool.
    pub fn pool(&self) -> &SqlitePool {
        self.pool.as_ref()
    }
}

async fn open_sqlite(path: &Path, migrator: &'static Migrator) -> anyhow::Result<SqlitePool> {
    let options = sqlite_connect_options_for_path(path);
    open_sqlite_with_options(options, migrator).await
}

async fn open_sqlite_with_options(
    options: SqliteConnectOptions,
    migrator: &'static Migrator,
) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    migrator.run(&pool).await?;
    // For existing databases the auto_vacuum mode is stored in the DB header
    // and cannot be changed by the connect option alone. Check and upgrade
    // on first open, then run an incremental vacuum pass on every open.
    let current: i64 = sqlx::query_scalar("PRAGMA auto_vacuum")
        .fetch_one(&pool)
        .await?;
    if current != SqliteAutoVacuum::Incremental as i64 {
        let _ = sqlx::query("PRAGMA auto_vacuum = INCREMENTAL")
            .execute(&pool)
            .await;
        // Full VACUUM is required to write the new mode into the DB header.
        // Best-effort: if another process holds a write lock, skip for now.
        let _ = sqlx::query("VACUUM").execute(&pool).await;
    }
    // Reclaim any pages freed since the last open. Best-effort.
    let _ = sqlx::query("PRAGMA incremental_vacuum")
        .execute(&pool)
        .await;
    Ok(pool)
}

fn sqlite_connect_options_for_path(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5))
        .auto_vacuum(SqliteAutoVacuum::Incremental)
        .log_statements(LevelFilter::Off)
}

fn sqlite_connect_options_for_url(database_url: &str) -> anyhow::Result<SqliteConnectOptions> {
    let options = SqliteConnectOptions::from_str(database_url)
        .map_err(|err| anyhow::anyhow!("invalid sqlite database URL: {err}"))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5))
        .auto_vacuum(SqliteAutoVacuum::Incremental)
        .log_statements(LevelFilter::Off);
    Ok(options)
}

pub fn runtime_db_filename() -> String {
    "chaos.sqlite".to_string()
}

pub fn runtime_db_path(chaos_home: &Path) -> PathBuf {
    chaos_home.join(runtime_db_filename())
}

pub async fn open_runtime_db(chaos_home: &Path) -> anyhow::Result<SqlitePool> {
    let runtime_path = runtime_db_path(chaos_home);
    open_runtime_db_at_path(runtime_path.as_path()).await
}

pub async fn open_runtime_db_at_path(path: &Path) -> anyhow::Result<SqlitePool> {
    open_sqlite(path, &STATE_MIGRATOR).await
}

pub async fn open_runtime_db_url(database_url: &str) -> anyhow::Result<SqlitePool> {
    let options = sqlite_connect_options_for_url(database_url)?;
    open_sqlite_with_options(options, &STATE_MIGRATOR).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    #[tokio::test]
    async fn runtime_db_uses_new_filename() {
        assert_eq!(runtime_db_filename(), "chaos.sqlite");
    }

    #[tokio::test]
    async fn open_runtime_db_creates_unified_runtime_schema() {
        let chaos_home = test_support::unique_temp_dir();
        tokio::fs::create_dir_all(&chaos_home)
            .await
            .expect("create temp chaos home");

        let pool = open_runtime_db(chaos_home.as_path())
            .await
            .expect("open runtime db");

        for table_name in [
            "processes",
            "process_leases",
            "journal_entries",
            "logs",
            "message_history",
            "backfill_state",
            "jobs",
            "stage1_outputs",
            "process_dynamic_tools",
            "agent_jobs",
            "agent_job_items",
            "process_spawn_edges",
            "cron_jobs",
            "model_catalog_cache",
        ] {
            let row =
                sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
                    .bind(table_name)
                    .fetch_one(&pool)
                    .await
                    .unwrap_or_else(|_| panic!("table {table_name} should exist"));

            let discovered_name: String = row.get("name");
            assert_eq!(discovered_name, table_name);
        }

        for view_name in [
            "due_cron_jobs",
            "valid_model_cache",
            "active_processes",
            "archived_processes",
            "active_process_leases",
            "process_message_counts",
        ] {
            let row =
                sqlx::query("SELECT name FROM sqlite_master WHERE type = 'view' AND name = ?")
                    .bind(view_name)
                    .fetch_one(&pool)
                    .await
                    .unwrap_or_else(|_| panic!("view {view_name} should exist"));

            let discovered_name: String = row.get("name");
            assert_eq!(discovered_name, view_name);
        }

        for trigger_name in [
            "cron_jobs_touch",
            "processes_touch",
            "process_leases_touch",
            "agent_jobs_touch",
            "agent_job_items_touch",
            "journal_entries_no_update",
            "journal_entries_no_delete",
        ] {
            let row =
                sqlx::query("SELECT name FROM sqlite_master WHERE type = 'trigger' AND name = ?")
                    .bind(trigger_name)
                    .fetch_one(&pool)
                    .await
                    .unwrap_or_else(|_| panic!("trigger {trigger_name} should exist"));

            let discovered_name: String = row.get("name");
            assert_eq!(discovered_name, trigger_name);
        }

        assert!(
            tokio::fs::try_exists(&runtime_db_path(chaos_home.as_path()))
                .await
                .expect("stat runtime db"),
            "runtime db file should be created on demand"
        );
    }

    #[tokio::test]
    async fn open_runtime_db_url_creates_schema() {
        let chaos_home = test_support::unique_temp_dir();
        tokio::fs::create_dir_all(&chaos_home)
            .await
            .expect("create temp chaos home");

        let db_path = runtime_db_path(chaos_home.as_path());
        let db_url = format!("sqlite://{}", db_path.display());
        let pool = open_runtime_db_url(&db_url)
            .await
            .expect("open runtime db from sqlite url");

        let row = sqlx::query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'cron_jobs'",
        )
        .fetch_one(&pool)
        .await
        .expect("cron_jobs table should exist");

        let table_name: String = row.get("name");
        assert_eq!(table_name, "cron_jobs");
        assert!(
            tokio::fs::try_exists(&db_path)
                .await
                .expect("stat runtime db"),
            "runtime db file should be created from sqlite url"
        );
    }

    #[tokio::test]
    async fn journal_entries_are_append_only() {
        let chaos_home = test_support::unique_temp_dir();
        tokio::fs::create_dir_all(&chaos_home)
            .await
            .expect("create temp chaos home");

        let pool = open_runtime_db(chaos_home.as_path())
            .await
            .expect("open runtime db");

        sqlx::query(
            "INSERT INTO processes (
                id, parent_process_id, fork_at_seq, source, source_json, model_provider, cwd,
                created_at, updated_at, archived_at, title, sandbox_policy, approval_mode,
                tokens_used, first_user_message, cli_version, agent_nickname, agent_role,
                git_sha, git_branch, git_origin_url, memory_mode, model, reasoning_effort,
                agent_path, process_name
            ) VALUES (?, NULL, NULL, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind("process-1")
        .bind("cli")
        .bind("\"cli\"")
        .bind("openai")
        .bind("/tmp")
        .bind(1_i64)
        .bind(1_i64)
        .bind("")
        .bind("")
        .bind("")
        .bind(0_i64)
        .bind("")
        .bind("")
        .bind("enabled")
        .execute(&pool)
        .await
        .expect("insert process");

        sqlx::query(
            "INSERT INTO journal_entries (process_id, seq, recorded_at, item_type, payload_json)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind("process-1")
        .bind(0_i64)
        .bind("2026-04-08T00:00:00Z")
        .bind("response_item")
        .bind("{\"ok\":true}")
        .execute(&pool)
        .await
        .expect("insert journal entry");

        let update_err = sqlx::query(
            "UPDATE journal_entries SET payload_json = ? WHERE process_id = ? AND seq = ?",
        )
        .bind("{\"ok\":false}")
        .bind("process-1")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect_err("journal entry update should fail");
        assert!(
            update_err.to_string().contains("append-only"),
            "unexpected update error: {update_err}"
        );

        let delete_err =
            sqlx::query("DELETE FROM journal_entries WHERE process_id = ? AND seq = ?")
                .bind("process-1")
                .bind(0_i64)
                .execute(&pool)
                .await
                .expect_err("journal entry delete should fail");
        assert!(
            delete_err.to_string().contains("append-only"),
            "unexpected delete error: {delete_err}"
        );
    }

    #[tokio::test]
    async fn processes_touch_trigger_updates_updated_at() {
        let chaos_home = test_support::unique_temp_dir();
        tokio::fs::create_dir_all(&chaos_home)
            .await
            .expect("create temp chaos home");

        let pool = open_runtime_db(chaos_home.as_path())
            .await
            .expect("open runtime db");

        sqlx::query(
            "INSERT INTO processes (
                id, parent_process_id, fork_at_seq, source, source_json, model_provider, cwd,
                created_at, updated_at, archived_at, title, sandbox_policy, approval_mode,
                tokens_used, first_user_message, cli_version, agent_nickname, agent_role,
                git_sha, git_branch, git_origin_url, memory_mode, model, reasoning_effort,
                agent_path, process_name
            ) VALUES (?, NULL, NULL, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind("process-1")
        .bind("cli")
        .bind("\"cli\"")
        .bind("openai")
        .bind("/tmp")
        .bind(1_i64)
        .bind(1_i64)
        .bind("")
        .bind("")
        .bind("")
        .bind(0_i64)
        .bind("")
        .bind("")
        .bind("enabled")
        .execute(&pool)
        .await
        .expect("insert process");

        sqlx::query("UPDATE processes SET title = ? WHERE id = ?")
            .bind("hello")
            .bind("process-1")
            .execute(&pool)
            .await
            .expect("update process title");

        let updated_at: i64 = sqlx::query_scalar("SELECT updated_at FROM processes WHERE id = ?")
            .bind("process-1")
            .fetch_one(&pool)
            .await
            .expect("fetch updated_at");
        assert!(updated_at > 1, "touch trigger should advance updated_at");
    }
}
