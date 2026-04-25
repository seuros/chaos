use crate::LogEntry;
use crate::LogQuery;
use crate::LogRow;
use crate::MinionJob;
use crate::MinionJobCreateParams;
use crate::MinionJobItem;
use crate::MinionJobItemCreateParams;
use crate::MinionJobItemStatus;
use crate::MinionJobProgress;
use crate::MinionJobStatus;
use crate::ProcessMetadata;
use crate::ProcessMetadataBuilder;
use crate::ProcessesPage;
use crate::SortKey;
use crate::apply_rollout_item;
use crate::migrations::POSTGRES_STATE_MIGRATOR;
use crate::migrations::STATE_MIGRATOR;
use crate::model::MinionJobRow;
use crate::model::ProcessRow;
use crate::model::anchor_from_item;
use crate::model::datetime_to_epoch_seconds;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::TrustLevel;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::protocol::RolloutItem;
use log::LevelFilter;
use serde_json::Value;
use sqlx::ConnectOptions;
use sqlx::PgConnection;
use sqlx::PgPool;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqliteConnection;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::postgres::PgRow;
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
use uuid::Uuid;
mod backfill;
mod logs;
mod memories;
mod message_history;
mod minion_jobs;
mod processes;
#[cfg(test)]
mod test_support;

// "Partition" is the retention bucket we cap at 10 MiB:
// - one bucket per non-null process_id
// - one bucket per processless (process_id IS NULL) non-null process_uuid
// - one bucket for processless rows with process_uuid IS NULL
const LOG_PARTITION_SIZE_LIMIT_BYTES: i64 = 10 * 1024 * 1024;
const LOG_PARTITION_ROW_LIMIT: i64 = 1_000;
const POSTGRES_POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct StateRuntime {
    chaos_home: PathBuf,
    default_provider: String,
    pool: Arc<sqlx::SqlitePool>,
}

#[derive(Clone)]
pub enum RuntimeDbHandle {
    Postgres(Arc<PostgresRuntime>),
    Sqlite(Arc<StateRuntime>),
}

#[derive(Clone)]
pub struct PostgresRuntime {
    chaos_home: PathBuf,
    default_provider: String,
    pool: PgPool,
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
        Ok(Self::from_sqlite_pool(
            chaos_home,
            default_provider,
            Arc::unwrap_or_clone(pool),
        ))
    }

    /// Build a runtime handle around an already-open SQLite pool.
    pub fn from_sqlite_pool(
        chaos_home: PathBuf,
        default_provider: String,
        pool: SqlitePool,
    ) -> Arc<Self> {
        Arc::new(Self {
            pool: Arc::new(pool),
            chaos_home,
            default_provider,
        })
    }

    /// Return the configured ChaOS home directory for this runtime.
    pub fn chaos_home(&self) -> &Path {
        self.chaos_home.as_path()
    }

    /// Return a reference to the runtime SQLite pool.
    pub fn pool(&self) -> &SqlitePool {
        self.pool.as_ref()
    }

    async fn get_project_trust(&self, project_path: &Path) -> anyhow::Result<Option<TrustLevel>> {
        let row = sqlx::query(
            r#"
SELECT trust_level
FROM project_trust
WHERE project_path = ?
            "#,
        )
        .bind(project_path.to_string_lossy().to_string())
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            let trust_level: String = row.try_get("trust_level")?;
            parse_trust_level(&trust_level)
        })
        .transpose()
    }

    async fn set_project_trust(
        &self,
        project_path: &Path,
        trust_level: TrustLevel,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO project_trust (
    project_path,
    trust_level,
    created_at,
    updated_at
) VALUES (
    ?,
    ?,
    UNIXEPOCH(),
    UNIXEPOCH()
)
ON CONFLICT(project_path) DO UPDATE
SET trust_level = excluded.trust_level,
    updated_at = UNIXEPOCH()
            "#,
        )
        .bind(project_path.to_string_lossy().to_string())
        .bind(trust_level.to_string())
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

impl RuntimeDbHandle {
    pub fn from_sqlite_pool(
        chaos_home: PathBuf,
        default_provider: String,
        pool: SqlitePool,
    ) -> Self {
        Self::Sqlite(StateRuntime::from_sqlite_pool(
            chaos_home,
            default_provider,
            pool,
        ))
    }

    pub fn from_postgres_pool(chaos_home: PathBuf, default_provider: String, pool: PgPool) -> Self {
        Self::Postgres(Arc::new(PostgresRuntime {
            chaos_home,
            default_provider,
            pool,
        }))
    }

    pub fn chaos_home(&self) -> &Path {
        match self {
            Self::Postgres(runtime) => runtime.chaos_home.as_path(),
            Self::Sqlite(runtime) => runtime.chaos_home(),
        }
    }

    pub fn sqlite_pool_cloned(&self) -> Option<SqlitePool> {
        match self {
            Self::Postgres(_) => None,
            Self::Sqlite(runtime) => Some(runtime.pool().to_owned()),
        }
    }

    pub async fn get_process(&self, id: ProcessId) -> anyhow::Result<Option<ProcessMetadata>> {
        match self {
            Self::Postgres(runtime) => runtime.get_process(id).await,
            Self::Sqlite(runtime) => runtime.get_process(id).await,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_processes(
        &self,
        page_size: usize,
        anchor: Option<&crate::Anchor>,
        sort_key: crate::SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
        search_term: Option<&str>,
    ) -> anyhow::Result<crate::ProcessesPage> {
        match self {
            Self::Postgres(runtime) => {
                runtime
                    .list_processes(
                        page_size,
                        anchor,
                        sort_key,
                        allowed_sources,
                        model_providers,
                        archived_only,
                        search_term,
                    )
                    .await
            }
            Self::Sqlite(runtime) => {
                runtime
                    .list_processes(
                        page_size,
                        anchor,
                        sort_key,
                        allowed_sources,
                        model_providers,
                        archived_only,
                        search_term,
                    )
                    .await
            }
        }
    }

    pub async fn list_process_ids(
        &self,
        limit: usize,
        anchor: Option<&crate::Anchor>,
        sort_key: crate::SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
    ) -> anyhow::Result<Vec<ProcessId>> {
        match self {
            Self::Postgres(runtime) => {
                runtime
                    .list_process_ids(
                        limit,
                        anchor,
                        sort_key,
                        allowed_sources,
                        model_providers,
                        archived_only,
                    )
                    .await
            }
            Self::Sqlite(runtime) => {
                runtime
                    .list_process_ids(
                        limit,
                        anchor,
                        sort_key,
                        allowed_sources,
                        model_providers,
                        archived_only,
                    )
                    .await
            }
        }
    }

    pub async fn get_dynamic_tools(
        &self,
        process_id: ProcessId,
    ) -> anyhow::Result<Option<Vec<DynamicToolSpec>>> {
        match self {
            Self::Postgres(runtime) => runtime.get_dynamic_tools(process_id).await,
            Self::Sqlite(runtime) => runtime.get_dynamic_tools(process_id).await,
        }
    }

    pub async fn persist_dynamic_tools(
        &self,
        process_id: ProcessId,
        tools: Option<&[DynamicToolSpec]>,
    ) -> anyhow::Result<()> {
        match self {
            Self::Postgres(runtime) => runtime.persist_dynamic_tools(process_id, tools).await,
            Self::Sqlite(runtime) => runtime.persist_dynamic_tools(process_id, tools).await,
        }
    }

    pub async fn get_project_trust(
        &self,
        project_path: &Path,
    ) -> anyhow::Result<Option<TrustLevel>> {
        match self {
            Self::Postgres(runtime) => runtime.get_project_trust(project_path).await,
            Self::Sqlite(runtime) => runtime.get_project_trust(project_path).await,
        }
    }

    pub async fn set_project_trust(
        &self,
        project_path: &Path,
        trust_level: TrustLevel,
    ) -> anyhow::Result<()> {
        match self {
            Self::Postgres(runtime) => runtime.set_project_trust(project_path, trust_level).await,
            Self::Sqlite(runtime) => runtime.set_project_trust(project_path, trust_level).await,
        }
    }

    pub async fn mark_process_memory_mode_polluted(
        &self,
        process_id: ProcessId,
    ) -> anyhow::Result<bool> {
        match self {
            Self::Postgres(runtime) => runtime.mark_process_memory_mode_polluted(process_id).await,
            Self::Sqlite(runtime) => runtime.mark_process_memory_mode_polluted(process_id).await,
        }
    }

    pub async fn apply_rollout_items(
        &self,
        builder: &ProcessMetadataBuilder,
        items: &[RolloutItem],
        new_process_memory_mode: Option<&str>,
        updated_at_override: Option<jiff::Timestamp>,
    ) -> anyhow::Result<()> {
        match self {
            Self::Postgres(runtime) => {
                runtime
                    .apply_rollout_items(
                        builder,
                        items,
                        new_process_memory_mode,
                        updated_at_override,
                    )
                    .await
            }
            Self::Sqlite(runtime) => {
                runtime
                    .apply_rollout_items(
                        builder,
                        items,
                        new_process_memory_mode,
                        updated_at_override,
                    )
                    .await
            }
        }
    }

    pub async fn touch_process_updated_at(
        &self,
        process_id: ProcessId,
        updated_at: jiff::Timestamp,
    ) -> anyhow::Result<bool> {
        match self {
            Self::Postgres(runtime) => {
                runtime
                    .touch_process_updated_at(process_id, updated_at)
                    .await
            }
            Self::Sqlite(runtime) => {
                runtime
                    .touch_process_updated_at(process_id, updated_at)
                    .await
            }
        }
    }

    pub async fn append_message_history_entry(
        &self,
        entry: &chaos_ipc::message_history::HistoryEntry,
        max_bytes: Option<usize>,
    ) -> anyhow::Result<()> {
        match self {
            Self::Postgres(runtime) => runtime.append_message_history_entry(entry, max_bytes).await,
            Self::Sqlite(runtime) => runtime.append_message_history_entry(entry, max_bytes).await,
        }
    }

    pub async fn message_history_metadata(&self) -> anyhow::Result<(u64, usize)> {
        match self {
            Self::Postgres(runtime) => runtime.message_history_metadata().await,
            Self::Sqlite(runtime) => runtime.message_history_metadata().await,
        }
    }

    pub async fn get_message_history_entry(
        &self,
        log_id: u64,
        offset: usize,
    ) -> anyhow::Result<Option<chaos_ipc::message_history::HistoryEntry>> {
        match self {
            Self::Postgres(runtime) => runtime.get_message_history_entry(log_id, offset).await,
            Self::Sqlite(runtime) => runtime.get_message_history_entry(log_id, offset).await,
        }
    }

    pub(crate) async fn create_minion_job(
        &self,
        params: &MinionJobCreateParams,
        items: &[MinionJobItemCreateParams],
    ) -> anyhow::Result<MinionJob> {
        match self {
            Self::Postgres(runtime) => runtime.create_minion_job(params, items).await,
            Self::Sqlite(runtime) => runtime.create_minion_job(params, items).await,
        }
    }

    pub(crate) async fn get_minion_job(&self, job_id: &str) -> anyhow::Result<Option<MinionJob>> {
        match self {
            Self::Postgres(runtime) => runtime.get_minion_job(job_id).await,
            Self::Sqlite(runtime) => runtime.get_minion_job(job_id).await,
        }
    }

    pub(crate) async fn list_minion_job_items(
        &self,
        job_id: &str,
        status: Option<MinionJobItemStatus>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<MinionJobItem>> {
        match self {
            Self::Postgres(runtime) => runtime.list_minion_job_items(job_id, status, limit).await,
            Self::Sqlite(runtime) => runtime.list_minion_job_items(job_id, status, limit).await,
        }
    }

    pub(crate) async fn get_minion_job_item(
        &self,
        job_id: &str,
        item_id: &str,
    ) -> anyhow::Result<Option<MinionJobItem>> {
        match self {
            Self::Postgres(runtime) => runtime.get_minion_job_item(job_id, item_id).await,
            Self::Sqlite(runtime) => runtime.get_minion_job_item(job_id, item_id).await,
        }
    }

    pub(crate) async fn mark_minion_job_running(&self, job_id: &str) -> anyhow::Result<()> {
        match self {
            Self::Postgres(runtime) => runtime.mark_minion_job_running(job_id).await,
            Self::Sqlite(runtime) => runtime.mark_minion_job_running(job_id).await,
        }
    }

    pub(crate) async fn mark_minion_job_completed(&self, job_id: &str) -> anyhow::Result<()> {
        match self {
            Self::Postgres(runtime) => runtime.mark_minion_job_completed(job_id).await,
            Self::Sqlite(runtime) => runtime.mark_minion_job_completed(job_id).await,
        }
    }

    pub(crate) async fn mark_minion_job_failed(
        &self,
        job_id: &str,
        error_message: &str,
    ) -> anyhow::Result<()> {
        match self {
            Self::Postgres(runtime) => runtime.mark_minion_job_failed(job_id, error_message).await,
            Self::Sqlite(runtime) => runtime.mark_minion_job_failed(job_id, error_message).await,
        }
    }

    pub(crate) async fn mark_minion_job_cancelled(
        &self,
        job_id: &str,
        reason: &str,
    ) -> anyhow::Result<bool> {
        match self {
            Self::Postgres(runtime) => runtime.mark_minion_job_cancelled(job_id, reason).await,
            Self::Sqlite(runtime) => runtime.mark_minion_job_cancelled(job_id, reason).await,
        }
    }

    pub(crate) async fn is_minion_job_cancelled(&self, job_id: &str) -> anyhow::Result<bool> {
        match self {
            Self::Postgres(runtime) => runtime.is_minion_job_cancelled(job_id).await,
            Self::Sqlite(runtime) => runtime.is_minion_job_cancelled(job_id).await,
        }
    }

    pub(crate) async fn mark_minion_job_item_running_with_thread(
        &self,
        job_id: &str,
        item_id: &str,
        process_id: &str,
    ) -> anyhow::Result<bool> {
        match self {
            Self::Postgres(runtime) => {
                runtime
                    .mark_minion_job_item_running_with_thread(job_id, item_id, process_id)
                    .await
            }
            Self::Sqlite(runtime) => {
                runtime
                    .mark_minion_job_item_running_with_thread(job_id, item_id, process_id)
                    .await
            }
        }
    }

    pub(crate) async fn mark_minion_job_item_pending(
        &self,
        job_id: &str,
        item_id: &str,
        error_message: Option<&str>,
    ) -> anyhow::Result<bool> {
        match self {
            Self::Postgres(runtime) => {
                runtime
                    .mark_minion_job_item_pending(job_id, item_id, error_message)
                    .await
            }
            Self::Sqlite(runtime) => {
                runtime
                    .mark_minion_job_item_pending(job_id, item_id, error_message)
                    .await
            }
        }
    }

    pub(crate) async fn report_minion_job_item_result(
        &self,
        job_id: &str,
        item_id: &str,
        reporting_process_id: &str,
        result_json: &Value,
    ) -> anyhow::Result<bool> {
        match self {
            Self::Postgres(runtime) => {
                runtime
                    .report_minion_job_item_result(
                        job_id,
                        item_id,
                        reporting_process_id,
                        result_json,
                    )
                    .await
            }
            Self::Sqlite(runtime) => {
                runtime
                    .report_minion_job_item_result(
                        job_id,
                        item_id,
                        reporting_process_id,
                        result_json,
                    )
                    .await
            }
        }
    }

    pub(crate) async fn mark_minion_job_item_completed(
        &self,
        job_id: &str,
        item_id: &str,
    ) -> anyhow::Result<bool> {
        match self {
            Self::Postgres(runtime) => {
                runtime
                    .mark_minion_job_item_completed(job_id, item_id)
                    .await
            }
            Self::Sqlite(runtime) => {
                runtime
                    .mark_minion_job_item_completed(job_id, item_id)
                    .await
            }
        }
    }

    pub(crate) async fn mark_minion_job_item_failed(
        &self,
        job_id: &str,
        item_id: &str,
        error_message: &str,
    ) -> anyhow::Result<bool> {
        match self {
            Self::Postgres(runtime) => {
                runtime
                    .mark_minion_job_item_failed(job_id, item_id, error_message)
                    .await
            }
            Self::Sqlite(runtime) => {
                runtime
                    .mark_minion_job_item_failed(job_id, item_id, error_message)
                    .await
            }
        }
    }

    pub(crate) async fn get_minion_job_progress(
        &self,
        job_id: &str,
    ) -> anyhow::Result<MinionJobProgress> {
        match self {
            Self::Postgres(runtime) => runtime.get_minion_job_progress(job_id).await,
            Self::Sqlite(runtime) => runtime.get_minion_job_progress(job_id).await,
        }
    }
}

impl AsRef<RuntimeDbHandle> for RuntimeDbHandle {
    fn as_ref(&self) -> &RuntimeDbHandle {
        self
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

async fn open_postgres_with_options(
    options: PgConnectOptions,
    migrator: &'static Migrator,
) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .acquire_timeout(POSTGRES_POOL_ACQUIRE_TIMEOUT)
        .max_connections(5)
        .connect_with(options)
        .await?;
    migrator.run(&pool).await?;
    Ok(pool)
}

fn postgres_connect_options_for_url(database_url: &str) -> anyhow::Result<PgConnectOptions> {
    let options = PgConnectOptions::from_str(database_url)
        .map_err(|err| anyhow::anyhow!("invalid postgres database URL: {err}"))?
        .log_statements(LevelFilter::Off);
    Ok(options)
}

pub async fn open_runtime_db_postgres_url(database_url: &str) -> anyhow::Result<PgPool> {
    let options = postgres_connect_options_for_url(database_url)?;
    open_postgres_with_options(options, &POSTGRES_STATE_MIGRATOR).await
}

impl PostgresRuntime {
    async fn get_project_trust(&self, project_path: &Path) -> anyhow::Result<Option<TrustLevel>> {
        let row = sqlx::query(
            r#"
SELECT trust_level
FROM project_trust
WHERE project_path = $1
            "#,
        )
        .bind(project_path.to_string_lossy().to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let trust_level: String = row.try_get("trust_level")?;
            parse_trust_level(&trust_level)
        })
        .transpose()
    }

    async fn set_project_trust(
        &self,
        project_path: &Path,
        trust_level: TrustLevel,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO project_trust (
    project_path,
    trust_level,
    created_at,
    updated_at
) VALUES (
    $1,
    $2,
    EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT
)
ON CONFLICT (project_path) DO UPDATE
SET trust_level = EXCLUDED.trust_level,
    updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(project_path.to_string_lossy().to_string())
        .bind(trust_level.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_process(&self, id: ProcessId) -> anyhow::Result<Option<crate::ProcessMetadata>> {
        let row = sqlx::query(
            r#"
SELECT
    id,
    created_at,
    updated_at,
    source,
    agent_nickname,
    agent_role,
    model_provider,
    cwd,
    cli_version,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    first_user_message,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url
FROM processes
WHERE id = $1
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(process_from_pg_row).transpose()
    }

    #[allow(clippy::too_many_arguments)]
    async fn list_processes(
        &self,
        page_size: usize,
        anchor: Option<&crate::Anchor>,
        sort_key: crate::SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
        search_term: Option<&str>,
    ) -> anyhow::Result<crate::ProcessesPage> {
        let limit = page_size.saturating_add(1);
        let mut builder = QueryBuilder::<sqlx::Postgres>::new(
            r#"
SELECT
    id,
    created_at,
    updated_at,
    source,
    agent_nickname,
    agent_role,
    model_provider,
    cwd,
    cli_version,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    first_user_message,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url
FROM processes
            "#,
        );
        push_process_filters_postgres(
            &mut builder,
            archived_only,
            allowed_sources,
            model_providers,
            anchor,
            sort_key,
            search_term,
        );
        push_process_order_and_limit_postgres(&mut builder, sort_key, limit);

        let rows = builder.build().fetch_all(&self.pool).await?;
        let mut items = rows
            .iter()
            .map(process_from_pg_row)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let num_scanned_rows = items.len();
        let next_anchor = if items.len() > page_size {
            items.pop();
            items
                .last()
                .and_then(|item| anchor_from_process(item, sort_key))
        } else {
            None
        };
        Ok(crate::ProcessesPage {
            items,
            next_anchor,
            num_scanned_rows,
        })
    }

    async fn list_process_ids(
        &self,
        limit: usize,
        anchor: Option<&crate::Anchor>,
        sort_key: crate::SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
    ) -> anyhow::Result<Vec<ProcessId>> {
        let mut builder = QueryBuilder::<sqlx::Postgres>::new("SELECT id FROM processes");
        push_process_filters_postgres(
            &mut builder,
            archived_only,
            allowed_sources,
            model_providers,
            anchor,
            sort_key,
            None,
        );
        push_process_order_and_limit_postgres(&mut builder, sort_key, limit);

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                Ok(ProcessId::try_from(id)?)
            })
            .collect()
    }

    async fn get_dynamic_tools(
        &self,
        process_id: ProcessId,
    ) -> anyhow::Result<Option<Vec<DynamicToolSpec>>> {
        let rows = sqlx::query(
            r#"
SELECT name, description, input_schema, defer_loading
FROM process_dynamic_tools
WHERE process_id = $1
ORDER BY position ASC
            "#,
        )
        .bind(process_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(None);
        }

        let mut tools = Vec::with_capacity(rows.len());
        for row in rows {
            tools.push(DynamicToolSpec {
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                input_schema: row.try_get("input_schema")?,
                defer_loading: row.try_get("defer_loading")?,
            });
        }
        Ok(Some(tools))
    }

    async fn persist_dynamic_tools(
        &self,
        process_id: ProcessId,
        tools: Option<&[DynamicToolSpec]>,
    ) -> anyhow::Result<()> {
        let Some(tools) = tools else {
            return Ok(());
        };
        if tools.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;
        let process_id = process_id.to_string();
        for (idx, tool) in tools.iter().enumerate() {
            let position = i64::try_from(idx).unwrap_or(i64::MAX);
            sqlx::query(
                r#"
INSERT INTO process_dynamic_tools (
    process_id,
    position,
    name,
    description,
    input_schema,
    defer_loading
) VALUES ($1, $2, $3, $4, $5, $6)
ON CONFLICT(process_id, position) DO NOTHING
                "#,
            )
            .bind(process_id.as_str())
            .bind(position)
            .bind(tool.name.as_str())
            .bind(tool.description.as_str())
            .bind(&tool.input_schema)
            .bind(tool.defer_loading)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn mark_process_memory_mode_polluted(
        &self,
        process_id: ProcessId,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE processes SET memory_mode = 'polluted' WHERE id = $1 AND memory_mode != 'polluted'",
        )
        .bind(process_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn apply_rollout_items(
        &self,
        builder: &ProcessMetadataBuilder,
        items: &[RolloutItem],
        new_process_memory_mode: Option<&str>,
        updated_at_override: Option<jiff::Timestamp>,
    ) -> anyhow::Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        let existing_metadata = self.get_process(builder.id).await?;
        let mut metadata = existing_metadata
            .clone()
            .unwrap_or_else(|| builder.build(&self.default_provider));
        for item in items {
            apply_rollout_item(&mut metadata, item, &self.default_provider);
        }
        if let Some(existing_metadata) = existing_metadata.as_ref() {
            metadata.prefer_existing_git_info(existing_metadata);
        }
        let updated_at = updated_at_override.unwrap_or_else(jiff::Timestamp::now);
        metadata.updated_at = updated_at;

        self.upsert_process(&metadata, new_process_memory_mode)
            .await?;

        if let Some(memory_mode) = extract_memory_mode(items) {
            let _ = sqlx::query("UPDATE processes SET memory_mode = $1 WHERE id = $2")
                .bind(memory_mode)
                .bind(builder.id.to_string())
                .execute(&self.pool)
                .await?;
        }
        if let Some(dynamic_tools) = extract_dynamic_tools(items) {
            self.persist_dynamic_tools(builder.id, dynamic_tools.as_deref())
                .await?;
        }
        Ok(())
    }

    async fn upsert_process(
        &self,
        metadata: &crate::ProcessMetadata,
        creation_memory_mode: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO processes (
    id,
    source_json,
    created_at,
    updated_at,
    source,
    agent_nickname,
    agent_role,
    model_provider,
    cwd,
    cli_version,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    first_user_message,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
) VALUES (
    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
    $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
)
ON CONFLICT(id) DO UPDATE SET
    source_json = excluded.source_json,
    created_at = excluded.created_at,
    updated_at = excluded.updated_at,
    source = excluded.source,
    agent_nickname = excluded.agent_nickname,
    agent_role = excluded.agent_role,
    model_provider = excluded.model_provider,
    cwd = excluded.cwd,
    cli_version = excluded.cli_version,
    title = excluded.title,
    sandbox_policy = excluded.sandbox_policy,
    approval_mode = excluded.approval_mode,
    tokens_used = excluded.tokens_used,
    first_user_message = excluded.first_user_message,
    archived_at = excluded.archived_at,
    git_sha = excluded.git_sha,
    git_branch = excluded.git_branch,
    git_origin_url = excluded.git_origin_url
            "#,
        )
        .bind(metadata.id.to_string())
        .bind(serde_json::Value::String(metadata.source.clone()))
        .bind(metadata.created_at.as_second())
        .bind(metadata.updated_at.as_second())
        .bind(metadata.source.as_str())
        .bind(metadata.agent_nickname.as_deref())
        .bind(metadata.agent_role.as_deref())
        .bind(metadata.model_provider.as_str())
        .bind(metadata.cwd.display().to_string())
        .bind(metadata.cli_version.as_str())
        .bind(metadata.title.as_str())
        .bind(metadata.sandbox_policy.as_str())
        .bind(metadata.approval_mode.as_str())
        .bind(metadata.tokens_used)
        .bind(metadata.first_user_message.as_deref().unwrap_or_default())
        .bind(metadata.archived_at.map(jiff::Timestamp::as_second))
        .bind(metadata.git_sha.as_deref())
        .bind(metadata.git_branch.as_deref())
        .bind(metadata.git_origin_url.as_deref())
        .bind(creation_memory_mode.unwrap_or("enabled"))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn touch_process_updated_at(
        &self,
        process_id: ProcessId,
        updated_at: jiff::Timestamp,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query("UPDATE processes SET updated_at = $1 WHERE id = $2")
            .bind(updated_at.as_second())
            .bind(process_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn append_message_history_entry(
        &self,
        entry: &chaos_ipc::message_history::HistoryEntry,
        max_bytes: Option<usize>,
    ) -> anyhow::Result<()> {
        let estimated_bytes = estimated_history_entry_bytes(entry)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
INSERT INTO message_history (conversation_id, ts, text, estimated_bytes)
VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(&entry.conversation_id)
        .bind(i64::try_from(entry.ts).unwrap_or(i64::MAX))
        .bind(&entry.text)
        .bind(estimated_bytes)
        .execute(&mut *tx)
        .await?;
        prune_message_history_after_insert_postgres(estimated_bytes, max_bytes, &mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn message_history_metadata(&self) -> anyhow::Result<(u64, usize)> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message_history")
            .fetch_one(&self.pool)
            .await?;
        Ok((
            self.message_history_log_id().await.unwrap_or(0),
            usize::try_from(count).unwrap_or(0),
        ))
    }

    async fn get_message_history_entry(
        &self,
        log_id: u64,
        offset: usize,
    ) -> anyhow::Result<Option<chaos_ipc::message_history::HistoryEntry>> {
        let current_log_id = self.message_history_log_id().await.unwrap_or(0);
        if log_id != 0 && current_log_id != 0 && current_log_id != log_id {
            return Ok(None);
        }

        let row = sqlx::query(
            r#"
SELECT conversation_id, ts, text
FROM message_history
ORDER BY id ASC
LIMIT 1 OFFSET $1
            "#,
        )
        .bind(i64::try_from(offset).unwrap_or(i64::MAX))
        .fetch_optional(&self.pool)
        .await?;

        row.map(
            |row| -> anyhow::Result<chaos_ipc::message_history::HistoryEntry> {
                Ok(chaos_ipc::message_history::HistoryEntry {
                    conversation_id: row.try_get("conversation_id")?,
                    ts: u64::try_from(row.try_get::<i64, _>("ts")?).unwrap_or(0),
                    text: row.try_get("text")?,
                })
            },
        )
        .transpose()
    }

    async fn create_minion_job(
        &self,
        params: &MinionJobCreateParams,
        items: &[MinionJobItemCreateParams],
    ) -> anyhow::Result<MinionJob> {
        let now = jiff::Timestamp::now().as_second();
        let input_headers_json = serde_json::to_value(&params.input_headers)?;
        let max_runtime_seconds = params
            .max_runtime_seconds
            .map(i64::try_from)
            .transpose()
            .map_err(|_| anyhow::anyhow!("invalid max_runtime_seconds value"))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
INSERT INTO agent_jobs (
    id,
    name,
    status,
    instruction,
    auto_export,
    max_runtime_seconds,
    output_schema_json,
    input_headers_json,
    input_csv_path,
    output_csv_path,
    created_at,
    updated_at,
    started_at,
    completed_at,
    last_error
) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NULL, NULL, NULL)
            "#,
        )
        .bind(params.id.as_str())
        .bind(params.name.as_str())
        .bind(MinionJobStatus::Pending.as_str())
        .bind(params.instruction.as_str())
        .bind(params.auto_export)
        .bind(max_runtime_seconds)
        .bind(params.output_schema_json.as_ref())
        .bind(&input_headers_json)
        .bind(params.input_csv_path.as_str())
        .bind(params.output_csv_path.as_str())
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        for item in items {
            sqlx::query(
                r#"
INSERT INTO agent_job_items (
    job_id,
    item_id,
    row_index,
    source_id,
    row_json,
    status,
    assigned_process_id,
    attempt_count,
    result_json,
    last_error,
    created_at,
    updated_at,
    completed_at,
    reported_at
) VALUES ($1, $2, $3, $4, $5, $6, NULL, 0, NULL, NULL, $7, $8, NULL, NULL)
                "#,
            )
            .bind(params.id.as_str())
            .bind(item.item_id.as_str())
            .bind(item.row_index)
            .bind(item.source_id.as_deref())
            .bind(&item.row_json)
            .bind(MinionJobItemStatus::Pending.as_str())
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        let job_id = params.id.as_str();
        self.get_minion_job(job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to load created minion job {job_id}"))
    }

    async fn get_minion_job(&self, job_id: &str) -> anyhow::Result<Option<MinionJob>> {
        let row = sqlx::query(
            r#"
SELECT
    id,
    name,
    status,
    instruction,
    CASE WHEN auto_export THEN 1 ELSE 0 END AS auto_export,
    max_runtime_seconds,
    output_schema_json::text AS output_schema_json,
    input_headers_json::text AS input_headers_json,
    input_csv_path,
    output_csv_path,
    created_at,
    updated_at,
    started_at,
    completed_at,
    last_error
FROM agent_jobs
WHERE id = $1
            "#,
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(minion_job_from_pg_row).transpose()
    }

    async fn list_minion_job_items(
        &self,
        job_id: &str,
        status: Option<MinionJobItemStatus>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<MinionJobItem>> {
        let mut builder = QueryBuilder::<sqlx::Postgres>::new(
            r#"
SELECT
    job_id,
    item_id,
    row_index,
    source_id,
    row_json::text AS row_json,
    status,
    assigned_process_id,
    attempt_count,
    result_json::text AS result_json,
    last_error,
    created_at,
    updated_at,
    completed_at,
    reported_at
FROM agent_job_items
WHERE job_id = 
            "#,
        );
        builder.push_bind(job_id);
        if let Some(status) = status {
            builder.push(" AND status = ");
            builder.push_bind(status.as_str());
        }
        builder.push(" ORDER BY row_index ASC");
        if let Some(limit) = limit {
            builder.push(" LIMIT ");
            builder.push_bind(limit as i64);
        }
        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.iter().map(minion_job_item_from_pg_row).collect()
    }

    async fn get_minion_job_item(
        &self,
        job_id: &str,
        item_id: &str,
    ) -> anyhow::Result<Option<MinionJobItem>> {
        let row = sqlx::query(
            r#"
SELECT
    job_id,
    item_id,
    row_index,
    source_id,
    row_json::text AS row_json,
    status,
    assigned_process_id,
    attempt_count,
    result_json::text AS result_json,
    last_error,
    created_at,
    updated_at,
    completed_at,
    reported_at
FROM agent_job_items
WHERE job_id = $1 AND item_id = $2
            "#,
        )
        .bind(job_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(minion_job_item_from_pg_row).transpose()
    }

    async fn mark_minion_job_running(&self, job_id: &str) -> anyhow::Result<()> {
        let status = self.get_minion_job_status(job_id).await?;
        anyhow::ensure!(
            status == MinionJobStatus::Pending,
            "cannot transition job {job_id} from {status:?} to Running"
        );

        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            r#"
UPDATE agent_jobs
SET
    status = $1,
    updated_at = $2,
    started_at = COALESCE(started_at, $3),
    completed_at = NULL,
    last_error = NULL
WHERE id = $4 AND status = $5
            "#,
        )
        .bind(MinionJobStatus::Running.as_str())
        .bind(now)
        .bind(now)
        .bind(job_id)
        .bind(status.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_minion_job_completed(&self, job_id: &str) -> anyhow::Result<()> {
        let status = self.get_minion_job_status(job_id).await?;
        anyhow::ensure!(
            status == MinionJobStatus::Running,
            "cannot transition job {job_id} from {status:?} to Completed"
        );

        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            r#"
UPDATE agent_jobs
SET status = $1, updated_at = $2, completed_at = $3, last_error = NULL
WHERE id = $4 AND status = $5
            "#,
        )
        .bind(MinionJobStatus::Completed.as_str())
        .bind(now)
        .bind(now)
        .bind(job_id)
        .bind(status.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_minion_job_failed(
        &self,
        job_id: &str,
        error_message: &str,
    ) -> anyhow::Result<()> {
        let status = self.get_minion_job_status(job_id).await?;
        anyhow::ensure!(
            matches!(status, MinionJobStatus::Pending | MinionJobStatus::Running),
            "cannot transition job {job_id} from {status:?} to Failed"
        );

        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            r#"
UPDATE agent_jobs
SET status = $1, updated_at = $2, completed_at = $3, last_error = $4
WHERE id = $5 AND status = $6
            "#,
        )
        .bind(MinionJobStatus::Failed.as_str())
        .bind(now)
        .bind(now)
        .bind(error_message)
        .bind(job_id)
        .bind(status.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_minion_job_cancelled(&self, job_id: &str, reason: &str) -> anyhow::Result<bool> {
        let status = self.get_minion_job_status(job_id).await?;
        if !matches!(status, MinionJobStatus::Pending | MinionJobStatus::Running) {
            return Ok(false);
        }

        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_jobs
SET status = $1, updated_at = $2, completed_at = $3, last_error = $4
WHERE id = $5 AND status = $6
            "#,
        )
        .bind(MinionJobStatus::Cancelled.as_str())
        .bind(now)
        .bind(now)
        .bind(reason)
        .bind(job_id)
        .bind(status.as_str())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_minion_job_status(&self, job_id: &str) -> anyhow::Result<MinionJobStatus> {
        let row = sqlx::query("SELECT status FROM agent_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await?;
        let row = row.ok_or_else(|| anyhow::anyhow!("minion job {job_id} not found"))?;
        let status: String = row.try_get("status")?;
        MinionJobStatus::parse(status.as_str())
    }

    async fn is_minion_job_cancelled(&self, job_id: &str) -> anyhow::Result<bool> {
        let row = sqlx::query(
            r#"
SELECT status
FROM agent_jobs
WHERE id = $1
            "#,
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let status: String = row.try_get("status")?;
        Ok(MinionJobStatus::parse(status.as_str())? == MinionJobStatus::Cancelled)
    }

    async fn mark_minion_job_item_running_with_thread(
        &self,
        job_id: &str,
        item_id: &str,
        process_id: &str,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    status = $1,
    assigned_process_id = $2,
    attempt_count = attempt_count + 1,
    updated_at = $3,
    last_error = NULL
WHERE job_id = $4 AND item_id = $5 AND status = $6
            "#,
        )
        .bind(MinionJobItemStatus::Running.as_str())
        .bind(process_id)
        .bind(now)
        .bind(job_id)
        .bind(item_id)
        .bind(MinionJobItemStatus::Pending.as_str())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn mark_minion_job_item_pending(
        &self,
        job_id: &str,
        item_id: &str,
        error_message: Option<&str>,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    status = $1,
    assigned_process_id = NULL,
    updated_at = $2,
    last_error = $3
WHERE job_id = $4 AND item_id = $5 AND status = $6
            "#,
        )
        .bind(MinionJobItemStatus::Pending.as_str())
        .bind(now)
        .bind(error_message)
        .bind(job_id)
        .bind(item_id)
        .bind(MinionJobItemStatus::Running.as_str())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn report_minion_job_item_result(
        &self,
        job_id: &str,
        item_id: &str,
        reporting_process_id: &str,
        result_json: &Value,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    result_json = $1,
    reported_at = $2,
    updated_at = $3,
    last_error = NULL
WHERE
    job_id = $4
    AND item_id = $5
    AND status = $6
    AND assigned_process_id = $7
            "#,
        )
        .bind(result_json)
        .bind(now)
        .bind(now)
        .bind(job_id)
        .bind(item_id)
        .bind(MinionJobItemStatus::Running.as_str())
        .bind(reporting_process_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn mark_minion_job_item_completed(
        &self,
        job_id: &str,
        item_id: &str,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    status = $1,
    completed_at = $2,
    updated_at = $3,
    assigned_process_id = NULL
WHERE
    job_id = $4
    AND item_id = $5
    AND status = $6
    AND result_json IS NOT NULL
            "#,
        )
        .bind(MinionJobItemStatus::Completed.as_str())
        .bind(now)
        .bind(now)
        .bind(job_id)
        .bind(item_id)
        .bind(MinionJobItemStatus::Running.as_str())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn mark_minion_job_item_failed(
        &self,
        job_id: &str,
        item_id: &str,
        error_message: &str,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    status = $1,
    completed_at = $2,
    updated_at = $3,
    last_error = $4,
    assigned_process_id = NULL
WHERE
    job_id = $5
    AND item_id = $6
    AND status = $7
            "#,
        )
        .bind(MinionJobItemStatus::Failed.as_str())
        .bind(now)
        .bind(now)
        .bind(error_message)
        .bind(job_id)
        .bind(item_id)
        .bind(MinionJobItemStatus::Running.as_str())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_minion_job_progress(&self, job_id: &str) -> anyhow::Result<MinionJobProgress> {
        let row = sqlx::query(
            r#"
SELECT
    COUNT(*) AS total_items,
    SUM(CASE WHEN status = $1 THEN 1 ELSE 0 END) AS pending_items,
    SUM(CASE WHEN status = $2 THEN 1 ELSE 0 END) AS running_items,
    SUM(CASE WHEN status = $3 THEN 1 ELSE 0 END) AS completed_items,
    SUM(CASE WHEN status = $4 THEN 1 ELSE 0 END) AS failed_items
FROM agent_job_items
WHERE job_id = $5
            "#,
        )
        .bind(MinionJobItemStatus::Pending.as_str())
        .bind(MinionJobItemStatus::Running.as_str())
        .bind(MinionJobItemStatus::Completed.as_str())
        .bind(MinionJobItemStatus::Failed.as_str())
        .bind(job_id)
        .fetch_one(&self.pool)
        .await?;

        let total_items: i64 = row.try_get("total_items")?;
        let pending_items: Option<i64> = row.try_get("pending_items")?;
        let running_items: Option<i64> = row.try_get("running_items")?;
        let completed_items: Option<i64> = row.try_get("completed_items")?;
        let failed_items: Option<i64> = row.try_get("failed_items")?;
        Ok(MinionJobProgress {
            total_items: usize::try_from(total_items).unwrap_or_default(),
            pending_items: usize::try_from(pending_items.unwrap_or_default()).unwrap_or_default(),
            running_items: usize::try_from(running_items.unwrap_or_default()).unwrap_or_default(),
            completed_items: usize::try_from(completed_items.unwrap_or_default())
                .unwrap_or_default(),
            failed_items: usize::try_from(failed_items.unwrap_or_default()).unwrap_or_default(),
        })
    }

    async fn message_history_log_id(&self) -> anyhow::Result<u64> {
        let database_oid: i64 = sqlx::query_scalar(
            "SELECT oid::bigint FROM pg_database WHERE datname = current_database()",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(u64::try_from(database_oid).unwrap_or(0))
    }
}

fn process_from_pg_row(row: &PgRow) -> anyhow::Result<crate::ProcessMetadata> {
    let first_user_message: String = row.try_get("first_user_message")?;
    let id: String = row.try_get("id")?;
    Ok(crate::ProcessMetadata {
        id: ProcessId::try_from(id)?,
        created_at: timestamp_from_epoch(row.try_get("created_at")?)?,
        updated_at: timestamp_from_epoch(row.try_get("updated_at")?)?,
        source: row.try_get("source")?,
        agent_nickname: row.try_get("agent_nickname")?,
        agent_role: row.try_get("agent_role")?,
        model_provider: row.try_get("model_provider")?,
        cwd: PathBuf::from(row.try_get::<String, _>("cwd")?),
        cli_version: row.try_get("cli_version")?,
        title: row.try_get("title")?,
        sandbox_policy: row.try_get("sandbox_policy")?,
        approval_mode: row.try_get("approval_mode")?,
        tokens_used: row.try_get("tokens_used")?,
        first_user_message: (!first_user_message.is_empty()).then_some(first_user_message),
        archived_at: row
            .try_get::<Option<i64>, _>("archived_at")?
            .map(timestamp_from_epoch)
            .transpose()?,
        git_sha: row.try_get("git_sha")?,
        git_branch: row.try_get("git_branch")?,
        git_origin_url: row.try_get("git_origin_url")?,
    })
}

fn minion_job_from_pg_row(row: &PgRow) -> anyhow::Result<MinionJob> {
    let output_schema_json = row
        .try_get::<Option<String>, _>("output_schema_json")?
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?;
    let input_headers_json: String = row.try_get("input_headers_json")?;
    let input_headers = serde_json::from_str(input_headers_json.as_str())?;
    let max_runtime_seconds = row
        .try_get::<Option<i64>, _>("max_runtime_seconds")?
        .map(u64::try_from)
        .transpose()
        .map_err(|_| anyhow::anyhow!("invalid max_runtime_seconds value"))?;
    let status: String = row.try_get("status")?;
    Ok(MinionJob {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        status: MinionJobStatus::parse(status.as_str())?,
        instruction: row.try_get("instruction")?,
        auto_export: row.try_get::<i64, _>("auto_export")? != 0,
        max_runtime_seconds,
        output_schema_json,
        input_headers,
        input_csv_path: row.try_get("input_csv_path")?,
        output_csv_path: row.try_get("output_csv_path")?,
        created_at: timestamp_from_epoch(row.try_get("created_at")?)?,
        updated_at: timestamp_from_epoch(row.try_get("updated_at")?)?,
        started_at: row
            .try_get::<Option<i64>, _>("started_at")?
            .map(timestamp_from_epoch)
            .transpose()?,
        completed_at: row
            .try_get::<Option<i64>, _>("completed_at")?
            .map(timestamp_from_epoch)
            .transpose()?,
        last_error: row.try_get("last_error")?,
    })
}

fn minion_job_item_from_pg_row(row: &PgRow) -> anyhow::Result<MinionJobItem> {
    let status: String = row.try_get("status")?;
    Ok(MinionJobItem {
        job_id: row.try_get("job_id")?,
        item_id: row.try_get("item_id")?,
        row_index: row.try_get("row_index")?,
        source_id: row.try_get("source_id")?,
        row_json: serde_json::from_str(row.try_get::<String, _>("row_json")?.as_str())?,
        status: MinionJobItemStatus::parse(status.as_str())?,
        assigned_process_id: row.try_get("assigned_process_id")?,
        attempt_count: row.try_get("attempt_count")?,
        result_json: row
            .try_get::<Option<String>, _>("result_json")?
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?,
        last_error: row.try_get("last_error")?,
        created_at: timestamp_from_epoch(row.try_get("created_at")?)?,
        updated_at: timestamp_from_epoch(row.try_get("updated_at")?)?,
        completed_at: row
            .try_get::<Option<i64>, _>("completed_at")?
            .map(timestamp_from_epoch)
            .transpose()?,
        reported_at: row
            .try_get::<Option<i64>, _>("reported_at")?
            .map(timestamp_from_epoch)
            .transpose()?,
    })
}

fn estimated_history_entry_bytes(
    entry: &chaos_ipc::message_history::HistoryEntry,
) -> anyhow::Result<i64> {
    let mut serialized = serde_json::to_string(entry)?;
    serialized.push('\n');
    Ok(i64::try_from(serialized.len()).unwrap_or(i64::MAX))
}

fn trim_target_bytes(max_bytes: i64, newest_entry_len: i64) -> i64 {
    const HISTORY_SOFT_CAP_RATIO: f64 = 0.8;

    let soft_cap_bytes = ((max_bytes as f64) * HISTORY_SOFT_CAP_RATIO)
        .floor()
        .clamp(1.0, max_bytes as f64) as i64;
    soft_cap_bytes.max(newest_entry_len)
}

async fn prune_message_history_after_insert_postgres(
    newest_entry_len: i64,
    max_bytes: Option<usize>,
    tx: &mut PgConnection,
) -> anyhow::Result<()> {
    let Some(max_bytes) = max_bytes else {
        return Ok(());
    };
    if max_bytes == 0 {
        return Ok(());
    }

    let max_bytes = i64::try_from(max_bytes).unwrap_or(i64::MAX);
    let total_bytes: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(estimated_bytes), 0) FROM message_history")
            .fetch_one(&mut *tx)
            .await?;

    if total_bytes <= max_bytes {
        return Ok(());
    }

    let trim_target = trim_target_bytes(max_bytes, newest_entry_len);
    sqlx::query(
        r#"
DELETE FROM message_history
WHERE id IN (
    SELECT id
    FROM (
        SELECT
            id,
            SUM(estimated_bytes) OVER (ORDER BY id DESC) AS cumulative_bytes
        FROM message_history
    ) ranked
    WHERE cumulative_bytes > $1
)
        "#,
    )
    .bind(trim_target)
    .execute(&mut *tx)
    .await?;

    Ok(())
}

fn timestamp_from_epoch(secs: i64) -> anyhow::Result<jiff::Timestamp> {
    jiff::Timestamp::from_second(secs)
        .map_err(|err| anyhow::anyhow!("invalid unix timestamp {secs}: {err}"))
}

fn anchor_from_process(
    item: &crate::ProcessMetadata,
    sort_key: crate::SortKey,
) -> Option<crate::Anchor> {
    let id = Uuid::parse_str(&item.id.to_string()).ok()?;
    let ts = match sort_key {
        crate::SortKey::CreatedAt => item.created_at,
        crate::SortKey::UpdatedAt => item.updated_at,
    };
    Some(crate::Anchor { ts, id })
}

fn extract_dynamic_tools(items: &[RolloutItem]) -> Option<Option<Vec<DynamicToolSpec>>> {
    items.iter().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => Some(meta_line.meta.dynamic_tools.clone()),
        RolloutItem::ResponseItem(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::EventMsg(_) => None,
    })
}

fn extract_memory_mode(items: &[RolloutItem]) -> Option<String> {
    items.iter().rev().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => meta_line.meta.memory_mode.clone(),
        RolloutItem::ResponseItem(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::EventMsg(_) => None,
    })
}

fn push_process_filters_postgres<'a>(
    builder: &mut QueryBuilder<'a, sqlx::Postgres>,
    archived_only: bool,
    allowed_sources: &'a [String],
    model_providers: Option<&'a [String]>,
    anchor: Option<&crate::Anchor>,
    sort_key: crate::SortKey,
    search_term: Option<&'a str>,
) {
    builder.push(" WHERE 1 = 1");
    if archived_only {
        builder.push(" AND archived_at IS NOT NULL");
    } else {
        builder.push(" AND archived_at IS NULL");
    }
    builder.push(" AND first_user_message <> ''");
    if !allowed_sources.is_empty() {
        builder.push(" AND source IN (");
        let mut separated = builder.separated(", ");
        for source in allowed_sources {
            separated.push_bind(source);
        }
        separated.push_unseparated(")");
    }
    if let Some(model_providers) = model_providers
        && !model_providers.is_empty()
    {
        builder.push(" AND model_provider IN (");
        let mut separated = builder.separated(", ");
        for provider in model_providers {
            separated.push_bind(provider);
        }
        separated.push_unseparated(")");
    }
    if let Some(search_term) = search_term {
        builder.push(" AND position(");
        builder.push_bind(search_term);
        builder.push(" in title) > 0");
    }
    if let Some(anchor) = anchor {
        let anchor_ts = anchor.ts.as_second();
        let column = match sort_key {
            crate::SortKey::CreatedAt => "created_at",
            crate::SortKey::UpdatedAt => "updated_at",
        };
        builder.push(" AND (");
        builder.push(column);
        builder.push(" < ");
        builder.push_bind(anchor_ts);
        builder.push(" OR (");
        builder.push(column);
        builder.push(" = ");
        builder.push_bind(anchor_ts);
        builder.push(" AND id < ");
        builder.push_bind(anchor.id.to_string());
        builder.push("))");
    }
}

fn push_process_order_and_limit_postgres(
    builder: &mut QueryBuilder<'_, sqlx::Postgres>,
    sort_key: crate::SortKey,
    limit: usize,
) {
    let order_column = match sort_key {
        crate::SortKey::CreatedAt => "created_at",
        crate::SortKey::UpdatedAt => "updated_at",
    };
    builder.push(" ORDER BY ");
    builder.push(order_column);
    builder.push(" DESC, id DESC");
    builder.push(" LIMIT ");
    builder.push_bind(limit as i64);
}

fn parse_trust_level(value: &str) -> anyhow::Result<TrustLevel> {
    match value {
        "trusted" => Ok(TrustLevel::Trusted),
        "untrusted" => Ok(TrustLevel::Untrusted),
        other => anyhow::bail!("invalid trust level `{other}` in runtime storage"),
    }
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
            "process_closure",
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
            "cron_jobs",
            "model_catalog_cache",
            "project_trust",
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
            "processes_parent_process_id_immutable",
            "processes_fork_at_seq_immutable",
            "processes_insert_closure",
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
    async fn project_trust_round_trips() {
        let chaos_home = test_support::unique_temp_dir();
        tokio::fs::create_dir_all(&chaos_home)
            .await
            .expect("create temp chaos home");

        let runtime = RuntimeDbHandle::Sqlite(
            StateRuntime::init(chaos_home, "test-provider".to_string())
                .await
                .expect("open runtime"),
        );
        let project_path = PathBuf::from("/tmp/trusted-project");

        assert_eq!(
            runtime
                .get_project_trust(&project_path)
                .await
                .expect("query trust"),
            None
        );

        runtime
            .set_project_trust(&project_path, TrustLevel::Trusted)
            .await
            .expect("set trust");
        assert_eq!(
            runtime
                .get_project_trust(&project_path)
                .await
                .expect("query trust"),
            Some(TrustLevel::Trusted)
        );

        runtime
            .set_project_trust(&project_path, TrustLevel::Untrusted)
            .await
            .expect("update trust");
        assert_eq!(
            runtime
                .get_project_trust(&project_path)
                .await
                .expect("query trust"),
            Some(TrustLevel::Untrusted)
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

    async fn insert_test_process(
        pool: &SqlitePool,
        id: &str,
        parent_process_id: Option<&str>,
        fork_at_seq: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO processes (
                id, parent_process_id, fork_at_seq, source, source_json, model_provider, cwd,
                created_at, updated_at, archived_at, title, sandbox_policy, approval_mode,
                tokens_used, first_user_message, cli_version, agent_nickname, agent_role,
                git_sha, git_branch, git_origin_url, memory_mode, model, reasoning_effort,
                agent_path, process_name
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind(id)
        .bind(parent_process_id)
        .bind(fork_at_seq)
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
        .execute(pool)
        .await
        .unwrap_or_else(|_| panic!("insert process {id}"));
    }

    #[tokio::test]
    async fn process_closure_rows_are_materialized_from_parent_links() {
        let chaos_home = test_support::unique_temp_dir();
        tokio::fs::create_dir_all(&chaos_home)
            .await
            .expect("create temp chaos home");

        let pool = open_runtime_db(chaos_home.as_path())
            .await
            .expect("open runtime db");

        insert_test_process(&pool, "root", None, None).await;
        insert_test_process(&pool, "child", Some("root"), Some(7)).await;
        insert_test_process(&pool, "grandchild", Some("child"), Some(3)).await;

        let closure_rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT ancestor_process_id, descendant_process_id, depth
             FROM process_closure
             ORDER BY ancestor_process_id, descendant_process_id",
        )
        .fetch_all(&pool)
        .await
        .expect("fetch process closure rows");

        assert_eq!(
            closure_rows,
            vec![
                ("child".to_string(), "child".to_string(), 0),
                ("child".to_string(), "grandchild".to_string(), 1),
                ("grandchild".to_string(), "grandchild".to_string(), 0),
                ("root".to_string(), "child".to_string(), 1),
                ("root".to_string(), "grandchild".to_string(), 2),
                ("root".to_string(), "root".to_string(), 0),
            ]
        );
    }

    #[tokio::test]
    async fn process_lineage_columns_are_immutable_after_insert() {
        let chaos_home = test_support::unique_temp_dir();
        tokio::fs::create_dir_all(&chaos_home)
            .await
            .expect("create temp chaos home");

        let pool = open_runtime_db(chaos_home.as_path())
            .await
            .expect("open runtime db");

        insert_test_process(&pool, "root", None, None).await;
        insert_test_process(&pool, "child", Some("root"), Some(7)).await;

        let parent_err = sqlx::query("UPDATE processes SET parent_process_id = ? WHERE id = ?")
            .bind::<Option<&str>>(None)
            .bind("child")
            .execute(&pool)
            .await
            .expect_err("updating parent_process_id should fail");
        assert!(
            parent_err.to_string().contains("immutable"),
            "unexpected parent immutability error: {parent_err}"
        );

        let fork_err = sqlx::query("UPDATE processes SET fork_at_seq = ? WHERE id = ?")
            .bind(8_i64)
            .bind("child")
            .execute(&pool)
            .await
            .expect_err("updating fork_at_seq should fail");
        assert!(
            fork_err.to_string().contains("immutable"),
            "unexpected fork_at_seq immutability error: {fork_err}"
        );
    }
}
