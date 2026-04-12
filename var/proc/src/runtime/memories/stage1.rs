use super::super::StateRuntime;
use super::{
    DEFAULT_RETRY_REMAINING, JOB_KIND_MEMORY_STAGE1, enqueue_global_consolidation_with_executor,
    whole_days_as_hours,
};
use super::{push_process_filters, push_process_order_and_limit};
use crate::model::{
    ProcessMetadata, ProcessRow, SortKey, Stage1JobClaim, Stage1JobClaimOutcome,
    Stage1StartupClaimParams,
};
use chaos_ipc::ProcessId;
use jiff::ToSpan;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use uuid::Uuid;

impl StateRuntime {
    /// Selects and claims stage-1 startup jobs for stale processes.
    ///
    /// Query behavior:
    /// - starts from `processes` filtered to active processes and allowed sources
    ///   (`push_process_filters`)
    /// - excludes processes with `memory_mode != 'enabled'`
    /// - excludes the current thread id
    /// - keeps only processes in the age window:
    ///   `updated_at >= now - max_age_days` and `updated_at <= now - min_rollout_idle_hours`
    /// - keeps only processes whose memory is stale:
    ///   `COALESCE(stage1_outputs.source_updated_at, -1) < processes.updated_at` and
    ///   `COALESCE(jobs.last_success_watermark, -1) < processes.updated_at`
    /// - orders by `updated_at DESC, id DESC` and applies `scan_limit`
    ///
    /// For each selected thread, this function calls [`Self::try_claim_stage1_job`]
    /// with `source_updated_at = thread.updated_at.as_second()` and returns up to
    /// `max_claimed` successful claims.
    pub async fn claim_stage1_jobs_for_startup(
        &self,
        current_process_id: ProcessId,
        params: Stage1StartupClaimParams<'_>,
    ) -> anyhow::Result<Vec<Stage1JobClaim>> {
        let Stage1StartupClaimParams {
            scan_limit,
            max_claimed,
            max_age_days,
            min_rollout_idle_hours,
            allowed_sources,
            lease_seconds,
        } = params;
        if scan_limit == 0 || max_claimed == 0 {
            return Ok(Vec::new());
        }

        let worker_id = current_process_id;
        let current_process_id = worker_id.to_string();
        let max_age_cutoff = jiff::Timestamp::now()
            .checked_sub(whole_days_as_hours(max_age_days.max(0)))
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
            .as_second();
        let idle_cutoff = jiff::Timestamp::now()
            .checked_sub(min_rollout_idle_hours.max(0).hours())
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
            .as_second();

        let mut builder = QueryBuilder::<Sqlite>::new(
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
LEFT JOIN stage1_outputs
    ON stage1_outputs.process_id = processes.id
LEFT JOIN jobs
    ON jobs.kind =
            "#,
        );
        builder.push_bind(JOB_KIND_MEMORY_STAGE1);
        builder.push(
            r#"
   AND jobs.job_key = processes.id
            "#,
        );
        push_process_filters(
            &mut builder,
            /*archived_only*/ false,
            allowed_sources,
            /*model_providers*/ None,
            /*anchor*/ None,
            SortKey::UpdatedAt,
            /*search_term*/ None,
        );
        builder.push(" AND processes.memory_mode = 'enabled'");
        builder
            .push(" AND id != ")
            .push_bind(current_process_id.as_str());
        builder
            .push(" AND updated_at >= ")
            .push_bind(max_age_cutoff);
        builder.push(" AND updated_at <= ").push_bind(idle_cutoff);
        builder.push(" AND COALESCE(stage1_outputs.source_updated_at, -1) < updated_at");
        builder.push(" AND COALESCE(jobs.last_success_watermark, -1) < updated_at");
        push_process_order_and_limit(&mut builder, SortKey::UpdatedAt, scan_limit);

        let items = builder
            .build()
            .fetch_all(self.pool.as_ref())
            .await?
            .into_iter()
            .map(|row| ProcessRow::try_from_row(&row).and_then(ProcessMetadata::try_from))
            .collect::<Result<Vec<_>, _>>()?;

        let mut claimed = Vec::new();

        for item in items {
            if claimed.len() >= max_claimed {
                break;
            }

            if let Stage1JobClaimOutcome::Claimed { ownership_token } = self
                .try_claim_stage1_job(
                    item.id,
                    worker_id,
                    item.updated_at.as_second(),
                    lease_seconds,
                    max_claimed,
                )
                .await?
            {
                claimed.push(Stage1JobClaim {
                    thread: item,
                    ownership_token,
                });
            }
        }

        Ok(claimed)
    }

    /// Attempts to claim a stage-1 job for a thread at `source_updated_at`.
    ///
    /// Claim semantics:
    /// - skips as up-to-date when either:
    ///   - `stage1_outputs.source_updated_at >= source_updated_at`, or
    ///   - `jobs.last_success_watermark >= source_updated_at`
    /// - inserts or updates a `jobs` row to `running` only when:
    ///   - global running job count for `memory_stage1` is below `max_running_jobs`
    ///   - existing row is not actively running with a valid lease
    ///   - retry backoff (if present) has elapsed, or `source_updated_at` advanced
    ///   - retries remain, or `source_updated_at` advanced (which resets retries)
    ///
    /// The update path refreshes ownership token, lease, and `input_watermark`.
    /// If claiming fails, a follow-up read maps current row state to a precise
    /// skip outcome (`SkippedRunning`, `SkippedRetryBackoff`, or
    /// `SkippedRetryExhausted`).
    pub async fn try_claim_stage1_job(
        &self,
        process_id: ProcessId,
        worker_id: ProcessId,
        source_updated_at: i64,
        lease_seconds: i64,
        max_running_jobs: usize,
    ) -> anyhow::Result<Stage1JobClaimOutcome> {
        let now = jiff::Timestamp::now().as_second();
        let lease_until = now.saturating_add(lease_seconds.max(0));
        let max_running_jobs = max_running_jobs as i64;
        let ownership_token = Uuid::new_v4().to_string();
        let process_id = process_id.to_string();
        let worker_id = worker_id.to_string();

        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        let existing_output = sqlx::query(
            r#"
SELECT source_updated_at
FROM stage1_outputs
WHERE process_id = ?
            "#,
        )
        .bind(process_id.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing_output) = existing_output {
            let existing_source_updated_at: i64 = existing_output.try_get("source_updated_at")?;
            if existing_source_updated_at >= source_updated_at {
                tx.commit().await?;
                return Ok(Stage1JobClaimOutcome::SkippedUpToDate);
            }
        }
        let existing_job = sqlx::query(
            r#"
SELECT last_success_watermark
FROM jobs
WHERE kind = ? AND job_key = ?
            "#,
        )
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(process_id.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing_job) = existing_job {
            let last_success_watermark =
                existing_job.try_get::<Option<i64>, _>("last_success_watermark")?;
            if last_success_watermark.is_some_and(|watermark| watermark >= source_updated_at) {
                tx.commit().await?;
                return Ok(Stage1JobClaimOutcome::SkippedUpToDate);
            }
        }

        let rows_affected = sqlx::query(
            r#"
INSERT INTO jobs (
    kind,
    job_key,
    status,
    worker_id,
    ownership_token,
    started_at,
    finished_at,
    lease_until,
    retry_at,
    retry_remaining,
    last_error,
    input_watermark,
    last_success_watermark
)
SELECT ?, ?, 'running', ?, ?, ?, NULL, ?, NULL, ?, NULL, ?, NULL
WHERE (
    SELECT COUNT(*)
    FROM jobs
    WHERE kind = ?
      AND status = 'running'
      AND lease_until IS NOT NULL
      AND lease_until > ?
) < ?
ON CONFLICT(kind, job_key) DO UPDATE SET
    status = 'running',
    worker_id = excluded.worker_id,
    ownership_token = excluded.ownership_token,
    started_at = excluded.started_at,
    finished_at = NULL,
    lease_until = excluded.lease_until,
    retry_at = NULL,
    retry_remaining = CASE
        WHEN excluded.input_watermark > COALESCE(jobs.input_watermark, -1) THEN ?
        ELSE jobs.retry_remaining
    END,
    last_error = NULL,
    input_watermark = excluded.input_watermark
WHERE
    (jobs.status != 'running' OR jobs.lease_until IS NULL OR jobs.lease_until <= excluded.started_at)
    AND (
        jobs.retry_at IS NULL
        OR jobs.retry_at <= excluded.started_at
        OR excluded.input_watermark > COALESCE(jobs.input_watermark, -1)
    )
    AND (
        jobs.retry_remaining > 0
        OR excluded.input_watermark > COALESCE(jobs.input_watermark, -1)
    )
    AND (
        SELECT COUNT(*)
        FROM jobs AS running_jobs
        WHERE running_jobs.kind = excluded.kind
          AND running_jobs.status = 'running'
          AND running_jobs.lease_until IS NOT NULL
          AND running_jobs.lease_until > excluded.started_at
          AND running_jobs.job_key != excluded.job_key
    ) < ?
            "#,
        )
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(process_id.as_str())
        .bind(worker_id.as_str())
        .bind(ownership_token.as_str())
        .bind(now)
        .bind(lease_until)
        .bind(DEFAULT_RETRY_REMAINING)
        .bind(source_updated_at)
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(now)
        .bind(max_running_jobs)
        .bind(DEFAULT_RETRY_REMAINING)
        .bind(max_running_jobs)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if rows_affected > 0 {
            tx.commit().await?;
            return Ok(Stage1JobClaimOutcome::Claimed { ownership_token });
        }

        let existing_job = sqlx::query(
            r#"
SELECT status, lease_until, retry_at, retry_remaining
FROM jobs
WHERE kind = ? AND job_key = ?
            "#,
        )
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(process_id.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        tx.commit().await?;

        if let Some(existing_job) = existing_job {
            let status: String = existing_job.try_get("status")?;
            let existing_lease_until: Option<i64> = existing_job.try_get("lease_until")?;
            let retry_at: Option<i64> = existing_job.try_get("retry_at")?;
            let retry_remaining: i64 = existing_job.try_get("retry_remaining")?;

            if retry_remaining <= 0 {
                return Ok(Stage1JobClaimOutcome::SkippedRetryExhausted);
            }
            if retry_at.is_some_and(|retry_at| retry_at > now) {
                return Ok(Stage1JobClaimOutcome::SkippedRetryBackoff);
            }
            if status == "running"
                && existing_lease_until.is_some_and(|lease_until| lease_until > now)
            {
                return Ok(Stage1JobClaimOutcome::SkippedRunning);
            }
        }

        Ok(Stage1JobClaimOutcome::SkippedRunning)
    }

    /// Marks a claimed stage-1 job successful and upserts generated output.
    ///
    /// Transaction behavior:
    /// - updates `jobs` only for the currently owned running row
    /// - sets `status='done'` and `last_success_watermark = input_watermark`
    /// - upserts `stage1_outputs` for the thread, replacing existing output only
    ///   when `source_updated_at` is newer or equal
    /// - preserves any existing `selected_for_phase2` baseline until the next
    ///   successful phase-2 run rewrites the baseline selection, including the
    ///   snapshot timestamp chosen during that run
    /// - persists optional `rollout_slug` for rollout summary artifact naming
    /// - enqueues/advances the global phase-2 job watermark using
    ///   `source_updated_at`
    pub async fn mark_stage1_job_succeeded(
        &self,
        process_id: ProcessId,
        ownership_token: &str,
        source_updated_at: i64,
        raw_memory: &str,
        rollout_summary: &str,
        rollout_slug: Option<&str>,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let process_id = process_id.to_string();

        let mut tx = self.pool.begin().await?;
        let rows_affected = sqlx::query(
            r#"
UPDATE jobs
SET
    status = 'done',
    finished_at = ?,
    lease_until = NULL,
    last_error = NULL,
    last_success_watermark = input_watermark
WHERE kind = ? AND job_key = ?
  AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(process_id.as_str())
        .bind(ownership_token)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if rows_affected == 0 {
            tx.commit().await?;
            return Ok(false);
        }

        sqlx::query(
            r#"
INSERT INTO stage1_outputs (
    process_id,
    source_updated_at,
    raw_memory,
    rollout_summary,
    rollout_slug,
    generated_at
) VALUES (?, ?, ?, ?, ?, ?)
ON CONFLICT(process_id) DO UPDATE SET
    source_updated_at = excluded.source_updated_at,
    raw_memory = excluded.raw_memory,
    rollout_summary = excluded.rollout_summary,
    rollout_slug = excluded.rollout_slug,
    generated_at = excluded.generated_at
WHERE excluded.source_updated_at >= stage1_outputs.source_updated_at
            "#,
        )
        .bind(process_id.as_str())
        .bind(source_updated_at)
        .bind(raw_memory)
        .bind(rollout_summary)
        .bind(rollout_slug)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        enqueue_global_consolidation_with_executor(&mut *tx, source_updated_at).await?;

        tx.commit().await?;
        Ok(true)
    }

    /// Marks a claimed stage-1 job successful when extraction produced no output.
    ///
    /// Transaction behavior:
    /// - updates `jobs` only for the currently owned running row
    /// - sets `status='done'` and `last_success_watermark = input_watermark`
    /// - deletes any existing `stage1_outputs` row for the thread
    /// - enqueues/advances the global phase-2 job watermark using the claimed
    ///   `input_watermark` only when deleting an existing `stage1_outputs` row
    pub async fn mark_stage1_job_succeeded_no_output(
        &self,
        process_id: ProcessId,
        ownership_token: &str,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let process_id = process_id.to_string();

        let mut tx = self.pool.begin().await?;
        let rows_affected = sqlx::query(
            r#"
UPDATE jobs
SET
    status = 'done',
    finished_at = ?,
    lease_until = NULL,
    last_error = NULL,
    last_success_watermark = input_watermark
WHERE kind = ? AND job_key = ?
  AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(process_id.as_str())
        .bind(ownership_token)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if rows_affected == 0 {
            tx.commit().await?;
            return Ok(false);
        }

        let source_updated_at = sqlx::query(
            r#"
SELECT input_watermark
FROM jobs
WHERE kind = ? AND job_key = ? AND ownership_token = ?
            "#,
        )
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(process_id.as_str())
        .bind(ownership_token)
        .fetch_one(&mut *tx)
        .await?
        .try_get::<i64, _>("input_watermark")?;

        let deleted_rows = sqlx::query(
            r#"
DELETE FROM stage1_outputs
WHERE process_id = ?
            "#,
        )
        .bind(process_id.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if deleted_rows > 0 {
            enqueue_global_consolidation_with_executor(&mut *tx, source_updated_at).await?;
        }

        tx.commit().await?;
        Ok(true)
    }

    /// Marks a claimed stage-1 job as failed and schedules retry backoff.
    ///
    /// Query behavior:
    /// - updates only the owned running row for `(kind='memory_stage1', job_key)`
    /// - sets `status='error'`, clears lease, writes `last_error`
    /// - decrements `retry_remaining`
    /// - sets `retry_at = now + retry_delay_seconds`
    pub async fn mark_stage1_job_failed(
        &self,
        process_id: ProcessId,
        ownership_token: &str,
        failure_reason: &str,
        retry_delay_seconds: i64,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let retry_at = now.saturating_add(retry_delay_seconds.max(0));
        let process_id = process_id.to_string();

        let rows_affected = sqlx::query(
            r#"
UPDATE jobs
SET
    status = 'error',
    finished_at = ?,
    lease_until = NULL,
    retry_at = ?,
    retry_remaining = retry_remaining - 1,
    last_error = ?
WHERE kind = ? AND job_key = ?
  AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(retry_at)
        .bind(failure_reason)
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(process_id.as_str())
        .bind(ownership_token)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();

        Ok(rows_affected > 0)
    }
}
