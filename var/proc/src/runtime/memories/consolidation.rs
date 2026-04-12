use super::super::StateRuntime;
use super::{
    DEFAULT_RETRY_REMAINING, JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL, MEMORY_CONSOLIDATION_JOB_KEY,
};
use crate::model::{Phase2JobClaimOutcome, Stage1Output};
use chaos_ipc::ProcessId;
use sqlx::Executor;
use sqlx::Row;
use sqlx::Sqlite;
use uuid::Uuid;

impl StateRuntime {
    /// Enqueues or advances the global phase-2 consolidation job watermark.
    ///
    /// The underlying upsert keeps the job `running` when already running, resets
    /// `pending/error` jobs to `pending`, and advances `input_watermark` so each
    /// enqueue marks new consolidation work even when `source_updated_at` is
    /// older than prior maxima.
    pub async fn enqueue_global_consolidation(&self, input_watermark: i64) -> anyhow::Result<()> {
        enqueue_global_consolidation_with_executor(self.pool.as_ref(), input_watermark).await
    }

    /// Attempts to claim the global phase-2 consolidation job.
    ///
    /// Claim semantics:
    /// - reads the singleton global job row (`kind='memory_consolidate_global'`)
    /// - returns `SkippedNotDirty` when `input_watermark <= last_success_watermark`
    /// - returns `SkippedNotDirty` when retries are exhausted or retry backoff is active
    /// - returns `SkippedRunning` when an active running lease exists
    /// - otherwise updates the row to `running`, sets ownership + lease, and
    ///   returns `Claimed`
    pub async fn try_claim_global_phase2_job(
        &self,
        worker_id: ProcessId,
        lease_seconds: i64,
    ) -> anyhow::Result<Phase2JobClaimOutcome> {
        let now = jiff::Timestamp::now().as_second();
        let lease_until = now.saturating_add(lease_seconds.max(0));
        let ownership_token = Uuid::new_v4().to_string();
        let worker_id = worker_id.to_string();

        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        let existing_job = sqlx::query(
            r#"
SELECT status, lease_until, retry_at, retry_remaining, input_watermark, last_success_watermark
FROM jobs
WHERE kind = ? AND job_key = ?
            "#,
        )
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(existing_job) = existing_job else {
            tx.commit().await?;
            return Ok(Phase2JobClaimOutcome::SkippedNotDirty);
        };

        let input_watermark: Option<i64> = existing_job.try_get("input_watermark")?;
        let input_watermark_value = input_watermark.unwrap_or(0);
        let last_success_watermark: Option<i64> = existing_job.try_get("last_success_watermark")?;
        if input_watermark_value <= last_success_watermark.unwrap_or(0) {
            tx.commit().await?;
            return Ok(Phase2JobClaimOutcome::SkippedNotDirty);
        }

        let status: String = existing_job.try_get("status")?;
        let existing_lease_until: Option<i64> = existing_job.try_get("lease_until")?;
        let retry_at: Option<i64> = existing_job.try_get("retry_at")?;
        let retry_remaining: i64 = existing_job.try_get("retry_remaining")?;

        if retry_remaining <= 0 {
            tx.commit().await?;
            return Ok(Phase2JobClaimOutcome::SkippedNotDirty);
        }
        if retry_at.is_some_and(|retry_at| retry_at > now) {
            tx.commit().await?;
            return Ok(Phase2JobClaimOutcome::SkippedNotDirty);
        }
        if status == "running" && existing_lease_until.is_some_and(|lease_until| lease_until > now)
        {
            tx.commit().await?;
            return Ok(Phase2JobClaimOutcome::SkippedRunning);
        }

        let rows_affected = sqlx::query(
            r#"
UPDATE jobs
SET
    status = 'running',
    worker_id = ?,
    ownership_token = ?,
    started_at = ?,
    finished_at = NULL,
    lease_until = ?,
    retry_at = NULL,
    last_error = NULL
WHERE kind = ? AND job_key = ?
  AND input_watermark > COALESCE(last_success_watermark, 0)
  AND (status != 'running' OR lease_until IS NULL OR lease_until <= ?)
  AND (retry_at IS NULL OR retry_at <= ?)
  AND retry_remaining > 0
            "#,
        )
        .bind(worker_id.as_str())
        .bind(ownership_token.as_str())
        .bind(now)
        .bind(lease_until)
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        tx.commit().await?;
        if rows_affected == 0 {
            Ok(Phase2JobClaimOutcome::SkippedRunning)
        } else {
            Ok(Phase2JobClaimOutcome::Claimed {
                ownership_token,
                input_watermark: input_watermark_value,
            })
        }
    }

    /// Extends the lease for an owned running phase-2 global job.
    ///
    /// Query behavior:
    /// - `UPDATE jobs SET lease_until = ?` for the singleton global row
    /// - requires `status='running'` and matching `ownership_token`
    pub async fn heartbeat_global_phase2_job(
        &self,
        ownership_token: &str,
        lease_seconds: i64,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let lease_until = now.saturating_add(lease_seconds.max(0));
        let rows_affected = sqlx::query(
            r#"
UPDATE jobs
SET lease_until = ?
WHERE kind = ? AND job_key = ?
  AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(lease_until)
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(ownership_token)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();

        Ok(rows_affected > 0)
    }

    /// Marks the owned running global phase-2 job as succeeded.
    ///
    /// Query behavior:
    /// - updates only the owned running singleton global row
    /// - sets `status='done'`, clears lease/errors
    /// - advances `last_success_watermark` to
    ///   `max(existing_last_success_watermark, completed_watermark)`
    /// - rewrites `selected_for_phase2` so only the exact selected stage-1
    ///   snapshots remain marked as part of the latest successful phase-2
    ///   selection, and persists each selected snapshot's
    ///   `source_updated_at` for future retained-vs-added diffing
    pub async fn mark_global_phase2_job_succeeded(
        &self,
        ownership_token: &str,
        completed_watermark: i64,
        selected_outputs: &[Stage1Output],
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let mut tx = self.pool.begin().await?;
        let rows_affected = sqlx::query(
            r#"
UPDATE jobs
SET
    status = 'done',
    finished_at = ?,
    lease_until = NULL,
    last_error = NULL,
    last_success_watermark = max(COALESCE(last_success_watermark, 0), ?)
WHERE kind = ? AND job_key = ?
  AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(completed_watermark)
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
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
UPDATE stage1_outputs
SET
    selected_for_phase2 = 0,
    selected_for_phase2_source_updated_at = NULL
WHERE selected_for_phase2 != 0 OR selected_for_phase2_source_updated_at IS NOT NULL
            "#,
        )
        .execute(&mut *tx)
        .await?;

        for output in selected_outputs {
            sqlx::query(
                r#"
UPDATE stage1_outputs
SET
    selected_for_phase2 = 1,
    selected_for_phase2_source_updated_at = ?
WHERE process_id = ? AND source_updated_at = ?
                "#,
            )
            .bind(output.source_updated_at.as_second())
            .bind(output.process_id.to_string())
            .bind(output.source_updated_at.as_second())
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(true)
    }

    /// Marks the owned running global phase-2 job as failed and schedules retry.
    ///
    /// Query behavior:
    /// - updates only the owned running singleton global row
    /// - sets `status='error'`, clears lease
    /// - writes failure reason and retry time
    /// - decrements `retry_remaining`
    pub async fn mark_global_phase2_job_failed(
        &self,
        ownership_token: &str,
        failure_reason: &str,
        retry_delay_seconds: i64,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let retry_at = now.saturating_add(retry_delay_seconds.max(0));
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
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(ownership_token)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();

        Ok(rows_affected > 0)
    }

    /// Fallback failure finalization when ownership may have been lost.
    ///
    /// Query behavior:
    /// - same state transition as [`Self::mark_global_phase2_job_failed`]
    /// - matches rows where `ownership_token = ? OR ownership_token IS NULL`
    /// - allows recovering a stuck unowned running row
    pub async fn mark_global_phase2_job_failed_if_unowned(
        &self,
        ownership_token: &str,
        failure_reason: &str,
        retry_delay_seconds: i64,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let retry_at = now.saturating_add(retry_delay_seconds.max(0));
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
  AND status = 'running'
  AND (ownership_token = ? OR ownership_token IS NULL)
            "#,
        )
        .bind(now)
        .bind(retry_at)
        .bind(failure_reason)
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(ownership_token)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();

        Ok(rows_affected > 0)
    }
}

pub(super) async fn enqueue_global_consolidation_with_executor<'e, E>(
    executor: E,
    input_watermark: i64,
) -> anyhow::Result<()>
where
    E: Executor<'e, Database = Sqlite>,
{
    sqlx::query(
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
) VALUES (?, ?, 'pending', NULL, NULL, NULL, NULL, NULL, NULL, ?, NULL, ?, 0)
ON CONFLICT(kind, job_key) DO UPDATE SET
    status = CASE
        WHEN jobs.status = 'running' THEN 'running'
        ELSE 'pending'
    END,
    retry_at = CASE
        WHEN jobs.status = 'running' THEN jobs.retry_at
        ELSE NULL
    END,
    retry_remaining = max(jobs.retry_remaining, excluded.retry_remaining),
    input_watermark = CASE
        WHEN excluded.input_watermark > COALESCE(jobs.input_watermark, 0)
            THEN excluded.input_watermark
        ELSE COALESCE(jobs.input_watermark, 0) + 1
    END
        "#,
    )
    .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
    .bind(MEMORY_CONSOLIDATION_JOB_KEY)
    .bind(DEFAULT_RETRY_REMAINING)
    .bind(input_watermark)
    .execute(executor)
    .await?;

    Ok(())
}
