//! Persistence for `spool_jobs` rows.
//!
//! Scope: the cron scheduler drives rows that are already `InProgress`.
//! Transition `Queued → InProgress` happens on the submit path (owned by
//! whatever tool creates the manifest) and is not implemented here.

use chaos_abi::SpoolEvent;
use chaos_abi::SpoolPhase;
use chaos_abi::SpoolState;
use chaos_vfs::ChaosVfs;
use chaos_vfs::Vfs;
use sqlx::PgPool;
use sqlx::SqlitePool;

mod saga;

#[derive(Debug, Clone, sqlx::FromRow)]
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
    pub saga_json: Option<String>,
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
        let row = sqlx::query_as::<_, SpoolRow>(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, \
                    created_at, updated_at, saga_json \
             FROM spool_jobs WHERE manifest_id = ?",
        )
        .bind(manifest_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Advance `row` by the polled `phase` and write the resulting terminal
    /// state. Returns `false` when the machine refuses the event or the row
    /// moved on since it was loaded.
    pub async fn mark_terminal(
        &self,
        row: &SpoolRow,
        phase: SpoolPhase,
        result_json: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<bool> {
        let Some(next) = advance(&row.status, phase.into()) else {
            return Ok(false);
        };
        let now = jiff::Timestamp::now().as_second();
        let updated = sqlx::query(
            "UPDATE spool_jobs SET status = ?, result_json = ?, error = ?, completed_at = ? \
             WHERE manifest_id = ? AND status = ? AND batch_id = ?",
        )
        .bind(next.name())
        .bind(result_json)
        .bind(error)
        .bind(now)
        .bind(&row.manifest_id)
        .bind(&row.status)
        .bind(row.batch_id.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
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
             VALUES (?, ?, NULL, ?, ?, ?, ?, ?) \
             ON CONFLICT (manifest_id) DO UPDATE SET \
               backend = excluded.backend, \
               batch_id = NULL, \
               status = excluded.status, \
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
        .bind(SpoolState::Queued.name())
        .bind(request_count as i64)
        .bind(payload_json)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fail a manifest whose submit saga aborted. Rows already terminal are
    /// left alone.
    pub async fn mark_submit_failed(&self, manifest_id: &str, error: &str) -> anyhow::Result<()> {
        loop {
            let Some(status) = sqlx::query_scalar::<_, String>(
                "SELECT status FROM spool_jobs WHERE manifest_id = ?",
            )
            .bind(manifest_id)
            .fetch_optional(&self.pool)
            .await?
            else {
                return Ok(());
            };
            let Some(next) = advance(&status, SpoolEvent::SubmitFailed) else {
                return Ok(());
            };
            let now = jiff::Timestamp::now().as_second();
            let updated = sqlx::query(
                "UPDATE spool_jobs SET status = ?, error = ?, completed_at = ? \
                 WHERE manifest_id = ? AND status = ?",
            )
            .bind(next.name())
            .bind(error)
            .bind(now)
            .bind(manifest_id)
            .bind(&status)
            .execute(&self.pool)
            .await?;
            if updated.rows_affected() == 1 {
                return Ok(());
            }
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
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "INSERT INTO spool_jobs \
             (manifest_id, backend, batch_id, status, request_count, payload_json, \
               submitted_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (manifest_id) DO UPDATE SET \
               backend = excluded.backend, \
               batch_id = excluded.batch_id, \
               status = excluded.status, \
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
        .bind(SpoolState::InProgress.name())
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
        let rows = sqlx::query_as::<_, SpoolRow>(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, \
                    created_at, updated_at, saga_json \
             FROM spool_jobs ORDER BY created_at DESC, manifest_id DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
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
        let row = sqlx::query_as::<_, SpoolRow>(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, \
                    created_at, updated_at, saga_json \
             FROM spool_jobs WHERE manifest_id = $1",
        )
        .bind(manifest_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Advance `row` by the polled `phase` and write the resulting terminal
    /// state. Returns `false` when the machine refuses the event or the row
    /// moved on since it was loaded.
    pub async fn mark_terminal(
        &self,
        row: &SpoolRow,
        phase: SpoolPhase,
        result_json: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<bool> {
        let Some(next) = advance(&row.status, phase.into()) else {
            return Ok(false);
        };
        let now = jiff::Timestamp::now().as_second();
        let updated = sqlx::query(
            "UPDATE spool_jobs SET status = $1, result_json = $2, error = $3, completed_at = $4 \
             WHERE manifest_id = $5 AND status = $6 AND batch_id = $7",
        )
        .bind(next.name())
        .bind(result_json)
        .bind(error)
        .bind(now)
        .bind(&row.manifest_id)
        .bind(&row.status)
        .bind(row.batch_id.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
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
             VALUES ($1, $2, NULL, $3, $4, $5, $6, $7) \
             ON CONFLICT (manifest_id) DO UPDATE SET \
                backend = EXCLUDED.backend, \
                batch_id = NULL, \
                status = excluded.status, \
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
        .bind(SpoolState::Queued.name())
        .bind(i64::from(request_count))
        .bind(payload_json)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fail a manifest whose submit saga aborted. Rows already terminal are
    /// left alone.
    pub async fn mark_submit_failed(&self, manifest_id: &str, error: &str) -> anyhow::Result<()> {
        loop {
            let Some(status) = sqlx::query_scalar::<_, String>(
                "SELECT status FROM spool_jobs WHERE manifest_id = $1",
            )
            .bind(manifest_id)
            .fetch_optional(&self.pool)
            .await?
            else {
                return Ok(());
            };
            let Some(next) = advance(&status, SpoolEvent::SubmitFailed) else {
                return Ok(());
            };
            let now = jiff::Timestamp::now().as_second();
            let updated = sqlx::query(
                "UPDATE spool_jobs SET status = $1, error = $2, completed_at = $3 \
                 WHERE manifest_id = $4 AND status = $5",
            )
            .bind(next.name())
            .bind(error)
            .bind(now)
            .bind(manifest_id)
            .bind(&status)
            .execute(&self.pool)
            .await?;
            if updated.rows_affected() == 1 {
                return Ok(());
            }
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
        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            "INSERT INTO spool_jobs \
             (manifest_id, backend, batch_id, status, request_count, payload_json, \
              submitted_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (manifest_id) DO UPDATE SET \
                backend = EXCLUDED.backend, \
                batch_id = EXCLUDED.batch_id, \
                status = excluded.status, \
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
        .bind(SpoolState::InProgress.name())
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
        let rows = sqlx::query_as::<_, SpoolRow>(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, \
                    created_at, updated_at, saga_json \
             FROM spool_jobs ORDER BY created_at DESC, manifest_id DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

#[derive(Clone)]
pub(crate) enum BackendSpoolStore {
    Sqlite(SpoolStore),
    Postgres(PostgresSpoolStore),
}

impl BackendSpoolStore {
    pub fn from_provider(vfs: &ChaosVfs) -> Self {
        match vfs.pool() {
            Vfs::Sqlite(pool) => Self::Sqlite(SpoolStore::new(pool)),
            Vfs::Postgres(pool) => Self::Postgres(PostgresSpoolStore::new(pool)),
        }
    }

    chaos_vfs::backend_dispatch! {
        pub async fn load(&self, manifest_id: &str) -> anyhow::Result<Option<SpoolRow>>;
        pub async fn mark_terminal(
            &self,
            row: &SpoolRow,
            phase: SpoolPhase,
            result_json: Option<&str>,
            error: Option<&str>,
        ) -> anyhow::Result<bool>;
        pub async fn insert_queued(
            &self,
            manifest_id: &str,
            backend: &str,
            request_count: u32,
            payload_json: &str,
        ) -> anyhow::Result<()>;
        pub async fn mark_submit_failed(
            &self,
            manifest_id: &str,
            error: &str,
        ) -> anyhow::Result<()>;
        pub async fn insert_submitted(
            &self,
            manifest_id: &str,
            backend: &str,
            batch_id: &str,
            request_count: u32,
            payload_json: &str,
        ) -> anyhow::Result<()>;
        pub async fn save_saga(
            &self,
            manifest_id: &str,
            saga_json: Option<&str>,
            batch_id: Option<&str>,
        ) -> anyhow::Result<()>;
        pub async fn list_pending_sagas(&self) -> anyhow::Result<Vec<SpoolRow>>;
        pub async fn list(&self) -> anyhow::Result<Vec<SpoolRow>>;
    }
}

fn advance(status: &str, event: SpoolEvent) -> Option<SpoolState> {
    SpoolState::from_name(status)?.after(event)
}
