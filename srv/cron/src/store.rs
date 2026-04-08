//! Persistence layer for cron jobs against chaos.sqlite.

use crate::job::CreateJobParams;
use crate::job::CronJob;
use crate::job::CronScope;
use crate::schedule::Schedule;
use sqlx::Row;
use sqlx::SqlitePool;

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
                "INSERT INTO cron_jobs (id, name, schedule, command, scope, project_path, session_id, enabled, next_run_at, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
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
        let mut query = String::from(
            "SELECT id, name, schedule, command, scope, project_path, session_id, enabled, last_run_at, next_run_at, created_at, updated_at FROM cron_jobs WHERE 1=1",
        );
        if scope.is_some() {
            query.push_str(" AND scope = ?");
        }
        if project_path.is_some() {
            query.push_str(" AND project_path = ?");
        }
        query.push_str(" ORDER BY created_at DESC");

        let mut q = sqlx::query(&query);
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
        let row = sqlx::query(
            "SELECT id, name, schedule, command, scope, project_path, session_id, enabled, last_run_at, next_run_at, created_at, updated_at FROM cron_jobs WHERE id = ?",
        )
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

    /// Fetch all enabled jobs whose next_run_at is at or before the given timestamp.
    /// Uses the `due_cron_jobs` view when querying for "right now", falls back to
    /// a parameterised query for arbitrary timestamps (tests, replay).
    pub async fn due_jobs(&self, now: i64) -> anyhow::Result<Vec<CronJob>> {
        let rows = sqlx::query(
            "SELECT id, name, schedule, command, scope, project_path, session_id, enabled, last_run_at, next_run_at, created_at, updated_at
             FROM cron_jobs
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
        let rows = sqlx::query(
            "SELECT id, name, schedule, command, scope, project_path, session_id, enabled, last_run_at, next_run_at, created_at, updated_at
             FROM due_cron_jobs",
        )
        .fetch_all(&self.pool)
        .await?;

        let jobs = rows.iter().map(row_to_job).collect();
        Ok(jobs)
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

fn row_to_job(row: &sqlx::sqlite::SqliteRow) -> CronJob {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_params(name: &str) -> CreateJobParams {
        CreateJobParams {
            name: name.to_string(),
            schedule: "1d".to_string(),
            command: "echo hi".to_string(),
            scope: CronScope::Project,
            project_path: None,
            session_id: None,
        }
    }

    #[tokio::test]
    async fn create_uses_short_hex_ids() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let pool = chaos_proc::open_runtime_db(temp_dir.path())
            .await
            .expect("open runtime db");
        let store = CronStore::new(pool);

        let job = store
            .create(&test_params("short-id"))
            .await
            .expect("create cron job");

        assert_eq!(job.id.len(), JOB_ID_HEX_CHARS);
        assert!(
            job.id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "id should be lowercase hex"
        );
    }

    #[tokio::test]
    async fn create_retries_on_id_collision() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let pool = chaos_proc::open_runtime_db(temp_dir.path())
            .await
            .expect("open runtime db");
        let store = CronStore::new(pool);

        let first = store
            .create_with_id_generator(&test_params("first"), || "deadbeef".to_string())
            .await
            .expect("create initial cron job");
        assert_eq!(first.id, "deadbeef");

        let mut ids = ["deadbeef", "cafebabe"].into_iter();
        let second = store
            .create_with_id_generator(&test_params("second"), || {
                ids.next().expect("have another id").to_string()
            })
            .await
            .expect("retry after collision");

        assert_eq!(second.id, "cafebabe");
    }

    #[tokio::test]
    async fn session_scoped_jobs_round_trip_session_id() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let pool = chaos_proc::open_runtime_db(temp_dir.path())
            .await
            .expect("open runtime db");
        let store = CronStore::new(pool.clone());

        let job = store
            .create(&CreateJobParams {
                name: "session-job".to_string(),
                schedule: "1d".to_string(),
                command: "echo hi".to_string(),
                scope: CronScope::Session,
                project_path: None,
                session_id: Some("session-123".to_string()),
            })
            .await
            .expect("create session cron job");

        let listed = store.list(None, None).await.expect("list cron jobs");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, job.id);
        assert_eq!(listed[0].session_id.as_deref(), Some("session-123"));

        let fetched = store
            .get(&job.id)
            .await
            .expect("get cron job")
            .expect("job exists");
        assert_eq!(fetched.session_id.as_deref(), Some("session-123"));

        sqlx::query("UPDATE cron_jobs SET next_run_at = 0 WHERE id = ?")
            .bind(&job.id)
            .execute(&pool)
            .await
            .expect("force job due");

        let due = store.due_jobs(1).await.expect("list due jobs");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].session_id.as_deref(), Some("session-123"));
    }

    #[tokio::test]
    async fn enabled_jobs_can_clear_next_run_at_after_running() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let pool = chaos_proc::open_runtime_db(temp_dir.path())
            .await
            .expect("open runtime db");
        let store = CronStore::new(pool);

        let job = store
            .create(&test_params("clear-next-run"))
            .await
            .expect("create cron job");

        store
            .mark_run(&job.id, None)
            .await
            .expect("mark job run without another occurrence");

        let refreshed = store
            .get(&job.id)
            .await
            .expect("get cron job")
            .expect("job exists");
        assert!(refreshed.enabled, "job should remain enabled");
        assert!(
            refreshed.last_run_at.is_some(),
            "last_run_at should be updated"
        );
        assert_eq!(refreshed.next_run_at, None);
    }

    #[tokio::test]
    async fn enabling_an_already_enabled_job_preserves_next_run_at() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let pool = chaos_proc::open_runtime_db(temp_dir.path())
            .await
            .expect("open runtime db");
        let store = CronStore::new(pool.clone());

        let job = store
            .create(&test_params("idempotent-enable"))
            .await
            .expect("create cron job");

        let expected_next_run_at = 4_242_424_242_i64;
        sqlx::query("UPDATE cron_jobs SET next_run_at = ? WHERE id = ?")
            .bind(expected_next_run_at)
            .bind(&job.id)
            .execute(&pool)
            .await
            .expect("set sentinel next_run_at");

        store
            .set_enabled(&job.id, true)
            .await
            .expect("re-enable already enabled job");

        let refreshed = store
            .get(&job.id)
            .await
            .expect("get cron job")
            .expect("job exists");
        assert!(refreshed.enabled, "job should remain enabled");
        assert_eq!(refreshed.next_run_at, Some(expected_next_run_at));
    }
}
