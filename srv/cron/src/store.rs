//! Persistence layer for cron jobs against the shared runtime DB.

use crate::job::CreateJobParams;
use crate::job::CronJob;
use crate::job::CronScope;
use crate::schedule::Schedule;
use sqlx::AssertSqlSafe;
use sqlx::PgPool;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;

const JOB_ID_HEX_CHARS: usize = 8;
const JOB_ID_GENERATION_ATTEMPTS: usize = 8;

/// Thin wrapper around the chaos sqlite pool for cron CRUD operations.
#[derive(Clone)]
pub struct CronStore {
    pool: SqlitePool,
}

impl CronStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a new cron job, computing the initial next_run_at. Returns the created job.
    pub async fn create(&self, params: &CreateJobParams) -> anyhow::Result<CronJob> {
        self.create_with_id_generator(params, generate_job_id).await
    }

    async fn create_with_id_generator<F>(
        &self,
        params: &CreateJobParams,
        mut next_id: F,
    ) -> anyhow::Result<CronJob>
    where
        F: FnMut() -> String,
    {
        let now_ts = jiff::Timestamp::now();
        let now = now_ts.as_second();
        let scope = params.scope.as_str();

        let parsed = Schedule::parse(&params.schedule)?;
        let next_run_at = parsed.next_after(now_ts).ok();

        for _ in 0..JOB_ID_GENERATION_ATTEMPTS {
            let id = next_id();

            match sqlx::query(
                "INSERT INTO cron_jobs (id, name, schedule, command, scope, project_path, session_id, enabled, next_run_at, created_at, updated_at, kind, manifest_id, execution_policy)
                 VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&params.name)
            .bind(&params.schedule)
            .bind(&params.command)
            .bind(scope)
            .bind(&params.project_path)
            .bind(&params.session_id)
            .bind(next_run_at)
            .bind(now)
            .bind(now)
            .bind(&params.kind)
            .bind(&params.manifest_id)
            .bind(&params.execution_policy)
            .execute(&self.pool)
            .await
            {
                Ok(_) => {
                    return Ok(CronJob {
                        id,
                        name: params.name.clone(),
                        schedule: params.schedule.clone(),
                        command: params.command.clone(),
                        scope: params.scope,
                        project_path: params.project_path.clone(),
                        session_id: params.session_id.clone(),
                        enabled: true,
                        last_run_at: None,
                        next_run_at,
                        created_at: now,
                        updated_at: now,
                        kind: params.kind.clone(),
                        manifest_id: params.manifest_id.clone(),
                        execution_policy: params.execution_policy.clone(),
                    });
                }
                Err(err) if is_unique_id_collision(&err) => continue,
                Err(err) => return Err(err.into()),
            }
        }

        anyhow::bail!(
            "failed to allocate unique cron job id after {JOB_ID_GENERATION_ATTEMPTS} attempts"
        )
    }

    /// List all jobs, optionally filtered by scope and/or project path.
    pub async fn list(
        &self,
        scope: Option<CronScope>,
        project_path: Option<&str>,
    ) -> anyhow::Result<Vec<CronJob>> {
        let mut query = String::from("SELECT * FROM cron_jobs WHERE 1=1");
        if scope.is_some() {
            query.push_str(" AND scope = ?");
        }
        if project_path.is_some() {
            query.push_str(" AND project_path = ?");
        }
        query.push_str(" ORDER BY created_at DESC");

        let mut q = sqlx::query(AssertSqlSafe(query));
        if let Some(ref s) = scope {
            q = q.bind(s.as_str());
        }
        if let Some(p) = project_path {
            q = q.bind(p);
        }

        let rows = q.fetch_all(&self.pool).await?;
        let jobs = rows.iter().map(row_to_job).collect();
        Ok(jobs)
    }

    /// Fetch a single job by ID.
    pub async fn get(&self, id: &str) -> anyhow::Result<Option<CronJob>> {
        let row = sqlx::query("SELECT * FROM cron_jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.as_ref().map(row_to_job))
    }

    /// Toggle a job's enabled state.
    /// When transitioning from disabled to enabled, recomputes `next_run_at`
    /// from the schedule. Re-applying the current state is a no-op.
    /// The `cron_jobs_touch` trigger auto-updates `updated_at`.
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        let job = self
            .get(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("job not found: {id}"))?;
        if job.enabled == enabled {
            return Ok(());
        }

        if enabled {
            let now_ts = jiff::Timestamp::now();
            let next_run_at = Schedule::parse(&job.schedule)
                .and_then(|s| s.next_after(now_ts))
                .ok();
            sqlx::query("UPDATE cron_jobs SET enabled = 1, next_run_at = ? WHERE id = ?")
                .bind(next_run_at)
                .bind(id)
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query("UPDATE cron_jobs SET enabled = 0 WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Record that a job just ran and set the next run time.
    /// The `cron_jobs_touch` trigger auto-updates `updated_at`.
    pub async fn mark_run(&self, id: &str, next_run_at: Option<i64>) -> anyhow::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        sqlx::query("UPDATE cron_jobs SET last_run_at = ?, next_run_at = ? WHERE id = ?")
            .bind(now)
            .bind(next_run_at)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Delete a job by ID.
    pub async fn delete(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM cron_jobs WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Delete spool-kind jobs bound to `manifest_id`, optionally preserving `keep_id`.
    pub async fn delete_spool_jobs_for_manifest_except(
        &self,
        manifest_id: &str,
        keep_id: Option<&str>,
    ) -> anyhow::Result<u64> {
        let result = match keep_id {
            Some(keep_id) => {
                sqlx::query(
                    "DELETE FROM cron_jobs \
                     WHERE kind = 'spool' AND manifest_id = ? AND id <> ?",
                )
                .bind(manifest_id)
                .bind(keep_id)
                .execute(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "DELETE FROM cron_jobs \
                     WHERE kind = 'spool' AND manifest_id = ?",
                )
                .bind(manifest_id)
                .execute(&self.pool)
                .await?
            }
        };
        Ok(result.rows_affected())
    }

    /// Fetch all enabled jobs whose next_run_at is at or before the given timestamp.
    /// Uses the `due_cron_jobs` view when querying for "right now", falls back to
    /// a parameterised query for arbitrary timestamps (tests, replay).
    pub async fn due_jobs(&self, now: i64) -> anyhow::Result<Vec<CronJob>> {
        let rows = sqlx::query(
            "SELECT * FROM cron_jobs
             WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?
             ORDER BY next_run_at ASC",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;

        let jobs = rows.iter().map(row_to_job).collect();
        Ok(jobs)
    }

    /// Fetch all jobs due right now using the `due_cron_jobs` view.
    pub async fn due_now(&self) -> anyhow::Result<Vec<CronJob>> {
        let rows = sqlx::query("SELECT * FROM due_cron_jobs")
            .fetch_all(&self.pool)
            .await?;

        let jobs = rows.iter().map(row_to_job).collect();
        Ok(jobs)
    }
}

/// Thin wrapper around the shared Postgres pool for cron CRUD operations.
#[derive(Clone)]
pub struct PostgresCronStore {
    pool: PgPool,
}

#[allow(dead_code)]
impl PostgresCronStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, params: &CreateJobParams) -> anyhow::Result<CronJob> {
        self.create_with_id_generator(params, generate_job_id).await
    }

    async fn create_with_id_generator<F>(
        &self,
        params: &CreateJobParams,
        mut next_id: F,
    ) -> anyhow::Result<CronJob>
    where
        F: FnMut() -> String,
    {
        let now_ts = jiff::Timestamp::now();
        let now = now_ts.as_second();
        let scope = params.scope.as_str();

        let parsed = Schedule::parse(&params.schedule)?;
        let next_run_at = parsed.next_after(now_ts).ok();

        for _ in 0..JOB_ID_GENERATION_ATTEMPTS {
            let id = next_id();

            match sqlx::query(
                "INSERT INTO cron_jobs (id, name, schedule, command, scope, project_path, session_id, enabled, next_run_at, created_at, updated_at, kind, manifest_id, execution_policy)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, TRUE, $8, $9, $10, $11, $12, $13)",
            )
            .bind(&id)
            .bind(&params.name)
            .bind(&params.schedule)
            .bind(&params.command)
            .bind(scope)
            .bind(&params.project_path)
            .bind(&params.session_id)
            .bind(next_run_at)
            .bind(now)
            .bind(now)
            .bind(&params.kind)
            .bind(&params.manifest_id)
            .bind(&params.execution_policy)
            .execute(&self.pool)
            .await
            {
                Ok(_) => {
                    return Ok(CronJob {
                        id,
                        name: params.name.clone(),
                        schedule: params.schedule.clone(),
                        command: params.command.clone(),
                        scope: params.scope,
                        project_path: params.project_path.clone(),
                        session_id: params.session_id.clone(),
                        enabled: true,
                        last_run_at: None,
                        next_run_at,
                        created_at: now,
                        updated_at: now,
                        kind: params.kind.clone(),
                        manifest_id: params.manifest_id.clone(),
                        execution_policy: params.execution_policy.clone(),
                    });
                }
                Err(err) if is_unique_id_collision(&err) => continue,
                Err(err) => return Err(err.into()),
            }
        }

        anyhow::bail!(
            "failed to allocate unique cron job id after {JOB_ID_GENERATION_ATTEMPTS} attempts"
        )
    }

    pub async fn list(
        &self,
        scope: Option<CronScope>,
        project_path: Option<&str>,
    ) -> anyhow::Result<Vec<CronJob>> {
        let rows = match (scope, project_path) {
            (Some(scope), Some(project_path)) => {
                sqlx::query(
                    "SELECT * FROM cron_jobs
                     WHERE scope = $1 AND project_path = $2
                     ORDER BY created_at DESC",
                )
                .bind(scope.as_str())
                .bind(project_path)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(scope), None) => {
                sqlx::query(
                    "SELECT * FROM cron_jobs
                     WHERE scope = $1
                     ORDER BY created_at DESC",
                )
                .bind(scope.as_str())
                .fetch_all(&self.pool)
                .await?
            }
            (None, Some(project_path)) => {
                sqlx::query(
                    "SELECT * FROM cron_jobs
                     WHERE project_path = $1
                     ORDER BY created_at DESC",
                )
                .bind(project_path)
                .fetch_all(&self.pool)
                .await?
            }
            (None, None) => {
                sqlx::query(
                    "SELECT * FROM cron_jobs
                     ORDER BY created_at DESC",
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.iter().map(row_to_job_postgres).collect())
    }

    pub async fn get(&self, id: &str) -> anyhow::Result<Option<CronJob>> {
        let row = sqlx::query("SELECT * FROM cron_jobs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.as_ref().map(row_to_job_postgres))
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        let job = self
            .get(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("job not found: {id}"))?;
        if job.enabled == enabled {
            return Ok(());
        }

        if enabled {
            let now_ts = jiff::Timestamp::now();
            let next_run_at = Schedule::parse(&job.schedule)
                .and_then(|s| s.next_after(now_ts))
                .ok();
            sqlx::query("UPDATE cron_jobs SET enabled = TRUE, next_run_at = $1 WHERE id = $2")
                .bind(next_run_at)
                .bind(id)
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query("UPDATE cron_jobs SET enabled = FALSE WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn mark_run(&self, id: &str, next_run_at: Option<i64>) -> anyhow::Result<()> {
        let now = jiff::Timestamp::now().as_second();
        sqlx::query("UPDATE cron_jobs SET last_run_at = $1, next_run_at = $2 WHERE id = $3")
            .bind(now)
            .bind(next_run_at)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM cron_jobs WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Delete spool-kind jobs bound to `manifest_id`, optionally preserving `keep_id`.
    pub async fn delete_spool_jobs_for_manifest_except(
        &self,
        manifest_id: &str,
        keep_id: Option<&str>,
    ) -> anyhow::Result<u64> {
        let result = match keep_id {
            Some(keep_id) => {
                sqlx::query(
                    "DELETE FROM cron_jobs \
                     WHERE kind = 'spool' AND manifest_id = $1 AND id <> $2",
                )
                .bind(manifest_id)
                .bind(keep_id)
                .execute(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "DELETE FROM cron_jobs \
                     WHERE kind = 'spool' AND manifest_id = $1",
                )
                .bind(manifest_id)
                .execute(&self.pool)
                .await?
            }
        };
        Ok(result.rows_affected())
    }

    pub async fn due_jobs(&self, now: i64) -> anyhow::Result<Vec<CronJob>> {
        let rows = sqlx::query(
            "SELECT * FROM cron_jobs
             WHERE enabled = TRUE AND next_run_at IS NOT NULL AND next_run_at <= $1
             ORDER BY next_run_at ASC",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(row_to_job_postgres).collect())
    }

    pub async fn due_now(&self) -> anyhow::Result<Vec<CronJob>> {
        let rows = sqlx::query("SELECT * FROM due_cron_jobs")
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.iter().map(row_to_job_postgres).collect())
    }
}

fn generate_job_id() -> String {
    let mut id = uuid::Uuid::new_v4().simple().to_string();
    id.truncate(JOB_ID_HEX_CHARS);
    id
}

fn is_unique_id_collision(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

fn row_to_job(row: &SqliteRow) -> CronJob {
    let scope_str: String = row.get("scope");
    CronJob {
        id: row.get("id"),
        name: row.get("name"),
        schedule: row.get("schedule"),
        command: row.get("command"),
        scope: scope_str.parse().unwrap_or(CronScope::Project),
        project_path: row.get("project_path"),
        session_id: row.get("session_id"),
        enabled: row.get::<i32, _>("enabled") != 0,
        last_run_at: row.get("last_run_at"),
        next_run_at: row.get("next_run_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        kind: row.get("kind"),
        manifest_id: row.get("manifest_id"),
        execution_policy: row.get("execution_policy"),
    }
}

fn row_to_job_postgres(row: &PgRow) -> CronJob {
    let scope_str: String = row.get("scope");
    CronJob {
        id: row.get("id"),
        name: row.get("name"),
        schedule: row.get("schedule"),
        command: row.get("command"),
        scope: scope_str.parse().unwrap_or(CronScope::Project),
        project_path: row.get("project_path"),
        session_id: row.get("session_id"),
        enabled: row.get("enabled"),
        last_run_at: row.get("last_run_at"),
        next_run_at: row.get("next_run_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        kind: row.get("kind"),
        manifest_id: row.get("manifest_id"),
        execution_policy: row.get("execution_policy"),
    }
}

#[cfg(test)]
mod tests;
