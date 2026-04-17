//! Persistence for `spool_jobs` rows.
//!
//! Scope: the cron scheduler drives rows that are already `InProgress`.
//! Transition `Queued → InProgress` happens on the submit path (owned by
//! whatever tool creates the manifest) and is not implemented here.

use chaos_abi::SpoolPhase;
use chaos_storage::ChaosStorageProvider;
use chaos_storage::StorageKind;
use sqlx::PgPool;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;

#[derive(Debug, Clone)]
pub(crate) struct SpoolRow {
    pub manifest_id: String,
    pub backend: String,
    pub batch_id: Option<String>,
    pub status: String,
    pub request_count: i64,
    pub payload_json: String,
    pub result_json: Option<String>,
    pub raw_result: Option<String>,
    pub error: Option<String>,
    pub submitted_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone)]
pub(crate) struct SpoolStore {
    pool: SqlitePool,
}

impl SpoolStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Load the row if it exists and has a non-null batch_id.
    pub async fn load(&self, manifest_id: &str) -> anyhow::Result<Option<SpoolRow>> {
        let row = sqlx::query(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, created_at, updated_at \
             FROM spool_jobs WHERE manifest_id = ?",
        )
        .bind(manifest_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(row_to_spool_row_sqlite(&row)))
    }

    /// Write terminal state: `Completed`/`Failed`/`Expired`/`Cancelled` with
    /// the corresponding payload columns.
    pub async fn mark_terminal(
        &self,
        manifest_id: &str,
        phase: SpoolPhase,
        result_json: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<()> {
        let status = phase_to_status(phase);
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "UPDATE spool_jobs SET status = ?, result_json = ?, error = ?, completed_at = ? \
             WHERE manifest_id = ?",
        )
        .bind(status)
        .bind(result_json)
        .bind(error)
        .bind(now)
        .bind(manifest_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert (or replace) a row in `InProgress` with the batch_id the backend
    /// handed back. Called from the submit path.
    pub async fn insert_queued(
        &self,
        manifest_id: &str,
        backend: &str,
        request_count: u32,
        payload_json: &str,
    ) -> anyhow::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "INSERT INTO spool_jobs \
             (manifest_id, backend, batch_id, status, request_count, payload_json, \
               created_at, updated_at) \
             VALUES (?, ?, NULL, 'Queued', ?, ?, ?, ?) \
             ON CONFLICT (manifest_id) DO UPDATE SET \
               backend = excluded.backend, \
               batch_id = NULL, \
               status = 'Queued', \
               request_count = excluded.request_count, \
               payload_json = excluded.payload_json, \
               result_json = NULL, \
               raw_result = NULL, \
               error = NULL, \
               submitted_at = NULL, \
               completed_at = NULL, \
               updated_at = excluded.updated_at",
        )
        .bind(manifest_id)
        .bind(backend)
        .bind(request_count as i64)
        .bind(payload_json)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a queued manifest as failed before it ever reached `InProgress`.
    pub async fn mark_submit_failed(&self, manifest_id: &str, error: &str) -> anyhow::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "UPDATE spool_jobs SET status = 'Failed', error = ?, completed_at = ? \
             WHERE manifest_id = ?",
        )
        .bind(error)
        .bind(now)
        .bind(manifest_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_submitted(
        &self,
        manifest_id: &str,
        backend: &str,
        batch_id: &str,
        request_count: u32,
        payload_json: &str,
    ) -> anyhow::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "INSERT INTO spool_jobs \
             (manifest_id, backend, batch_id, status, request_count, payload_json, \
               submitted_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'InProgress', ?, ?, ?, ?, ?) \
             ON CONFLICT (manifest_id) DO UPDATE SET \
               backend = excluded.backend, \
               batch_id = excluded.batch_id, \
               status = 'InProgress', \
               request_count = excluded.request_count, \
               payload_json = excluded.payload_json, \
               result_json = NULL, \
               raw_result = NULL, \
               error = NULL, \
               submitted_at = excluded.submitted_at, \
               completed_at = NULL, \
               updated_at = excluded.updated_at",
        )
        .bind(manifest_id)
        .bind(backend)
        .bind(batch_id)
        .bind(request_count as i64)
        .bind(payload_json)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self) -> anyhow::Result<Vec<SpoolRow>> {
        let rows = sqlx::query(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, created_at, updated_at \
             FROM spool_jobs ORDER BY created_at DESC, manifest_id DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_spool_row_sqlite).collect())
    }
}

#[derive(Clone)]
pub(crate) struct PostgresSpoolStore {
    pool: PgPool,
}

impl PostgresSpoolStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Load the row if it exists and has a non-null batch_id.
    pub async fn load(&self, manifest_id: &str) -> anyhow::Result<Option<SpoolRow>> {
        let row = sqlx::query(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, created_at, updated_at \
             FROM spool_jobs WHERE manifest_id = $1",
        )
        .bind(manifest_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(row_to_spool_row_postgres(&row)))
    }

    /// Write terminal state: `Completed`/`Failed`/`Expired`/`Cancelled` with
    /// the corresponding payload columns.
    pub async fn mark_terminal(
        &self,
        manifest_id: &str,
        phase: SpoolPhase,
        result_json: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<()> {
        let status = phase_to_status(phase);
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "UPDATE spool_jobs SET status = $1, result_json = $2, error = $3, completed_at = $4 \
             WHERE manifest_id = $5",
        )
        .bind(status)
        .bind(result_json)
        .bind(error)
        .bind(now)
        .bind(manifest_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert (or upsert) a row in `InProgress` with the batch_id the backend
    /// handed back. Called from the submit path.
    pub async fn insert_queued(
        &self,
        manifest_id: &str,
        backend: &str,
        request_count: u32,
        payload_json: &str,
    ) -> anyhow::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "INSERT INTO spool_jobs \
             (manifest_id, backend, batch_id, status, request_count, payload_json, \
              created_at, updated_at) \
             VALUES ($1, $2, NULL, 'Queued', $3, $4, $5, $6) \
             ON CONFLICT (manifest_id) DO UPDATE SET \
                backend = EXCLUDED.backend, \
                batch_id = NULL, \
                status = 'Queued', \
                request_count = EXCLUDED.request_count, \
                payload_json = EXCLUDED.payload_json, \
                result_json = NULL, \
                raw_result = NULL, \
                error = NULL, \
                submitted_at = NULL, \
                completed_at = NULL, \
                updated_at = EXCLUDED.updated_at",
        )
        .bind(manifest_id)
        .bind(backend)
        .bind(i64::from(request_count))
        .bind(payload_json)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a queued manifest as failed before it ever reached `InProgress`.
    pub async fn mark_submit_failed(&self, manifest_id: &str, error: &str) -> anyhow::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "UPDATE spool_jobs SET status = 'Failed', error = $1, completed_at = $2 \
             WHERE manifest_id = $3",
        )
        .bind(error)
        .bind(now)
        .bind(manifest_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_submitted(
        &self,
        manifest_id: &str,
        backend: &str,
        batch_id: &str,
        request_count: u32,
        payload_json: &str,
    ) -> anyhow::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "INSERT INTO spool_jobs \
             (manifest_id, backend, batch_id, status, request_count, payload_json, \
              submitted_at, created_at, updated_at) \
             VALUES ($1, $2, $3, 'InProgress', $4, $5, $6, $7, $8) \
             ON CONFLICT (manifest_id) DO UPDATE SET \
                backend = EXCLUDED.backend, \
                batch_id = EXCLUDED.batch_id, \
                status = 'InProgress', \
                request_count = EXCLUDED.request_count, \
                payload_json = EXCLUDED.payload_json, \
                result_json = NULL, \
                raw_result = NULL, \
                error = NULL, \
                submitted_at = EXCLUDED.submitted_at, \
                completed_at = NULL, \
                updated_at = EXCLUDED.updated_at",
        )
        .bind(manifest_id)
        .bind(backend)
        .bind(batch_id)
        .bind(i64::from(request_count))
        .bind(payload_json)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self) -> anyhow::Result<Vec<SpoolRow>> {
        let rows = sqlx::query(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, created_at, updated_at \
             FROM spool_jobs ORDER BY created_at DESC, manifest_id DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_spool_row_postgres).collect())
    }
}

#[derive(Clone)]
pub(crate) enum BackendSpoolStore {
    Sqlite(SpoolStore),
    Postgres(PostgresSpoolStore),
}

impl BackendSpoolStore {
    pub fn from_provider(provider: &ChaosStorageProvider) -> Result<Self, String> {
        match provider.kind() {
            StorageKind::Sqlite => {
                let pool = provider.sqlite_pool_cloned().ok_or_else(|| {
                    "chaos DB unavailable — spool storage backend not supported".to_string()
                })?;
                Ok(Self::Sqlite(SpoolStore::new(pool)))
            }
            StorageKind::Postgres => {
                let pool = provider.postgres_pool_cloned().ok_or_else(|| {
                    "chaos DB unavailable — spool storage backend not supported".to_string()
                })?;
                Ok(Self::Postgres(PostgresSpoolStore::new(pool)))
            }
        }
    }

    pub async fn load(&self, manifest_id: &str) -> anyhow::Result<Option<SpoolRow>> {
        match self {
            Self::Sqlite(store) => store.load(manifest_id).await,
            Self::Postgres(store) => store.load(manifest_id).await,
        }
    }

    pub async fn mark_terminal(
        &self,
        manifest_id: &str,
        phase: SpoolPhase,
        result_json: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<()> {
        match self {
            Self::Sqlite(store) => {
                store
                    .mark_terminal(manifest_id, phase, result_json, error)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .mark_terminal(manifest_id, phase, result_json, error)
                    .await
            }
        }
    }

    pub async fn insert_queued(
        &self,
        manifest_id: &str,
        backend: &str,
        request_count: u32,
        payload_json: &str,
    ) -> anyhow::Result<()> {
        match self {
            Self::Sqlite(store) => {
                store
                    .insert_queued(manifest_id, backend, request_count, payload_json)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .insert_queued(manifest_id, backend, request_count, payload_json)
                    .await
            }
        }
    }

    pub async fn mark_submit_failed(&self, manifest_id: &str, error: &str) -> anyhow::Result<()> {
        match self {
            Self::Sqlite(store) => store.mark_submit_failed(manifest_id, error).await,
            Self::Postgres(store) => store.mark_submit_failed(manifest_id, error).await,
        }
    }

    pub async fn insert_submitted(
        &self,
        manifest_id: &str,
        backend: &str,
        batch_id: &str,
        request_count: u32,
        payload_json: &str,
    ) -> anyhow::Result<()> {
        match self {
            Self::Sqlite(store) => {
                store
                    .insert_submitted(manifest_id, backend, batch_id, request_count, payload_json)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .insert_submitted(manifest_id, backend, batch_id, request_count, payload_json)
                    .await
            }
        }
    }

    pub async fn list(&self) -> anyhow::Result<Vec<SpoolRow>> {
        match self {
            Self::Sqlite(store) => store.list().await,
            Self::Postgres(store) => store.list().await,
        }
    }
}

fn phase_to_status(phase: SpoolPhase) -> &'static str {
    match phase {
        SpoolPhase::InProgress => "InProgress",
        SpoolPhase::Completed => "Completed",
        SpoolPhase::Failed => "Failed",
        SpoolPhase::Expired => "Expired",
        SpoolPhase::Cancelled => "Cancelled",
    }
}

fn row_to_spool_row_sqlite(row: &SqliteRow) -> SpoolRow {
    SpoolRow {
        manifest_id: row.get("manifest_id"),
        backend: row.get("backend"),
        batch_id: row.get("batch_id"),
        status: row.get("status"),
        request_count: row.get("request_count"),
        payload_json: row.get("payload_json"),
        result_json: row.get("result_json"),
        raw_result: row.get("raw_result"),
        error: row.get("error"),
        submitted_at: row.get("submitted_at"),
        completed_at: row.get("completed_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn row_to_spool_row_postgres(row: &PgRow) -> SpoolRow {
    SpoolRow {
        manifest_id: row.get("manifest_id"),
        backend: row.get("backend"),
        batch_id: row.get("batch_id"),
        status: row.get("status"),
        request_count: row.get("request_count"),
        payload_json: row.get("payload_json"),
        result_json: row.get("result_json"),
        raw_result: row.get("raw_result"),
        error: row.get("error"),
        submitted_at: row.get("submitted_at"),
        completed_at: row.get("completed_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
