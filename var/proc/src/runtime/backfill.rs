use super::StateRuntime;
use crate::model::backfill_machine::BackfillWorkflow;

impl StateRuntime {
    pub(crate) async fn get_backfill_state(&self) -> anyhow::Result<crate::BackfillState> {
        self.ensure_backfill_state_row().await?;
        let row = sqlx::query(
            r#"
SELECT status, last_watermark, last_success_at
FROM backfill_state
WHERE id = 1
            "#,
        )
        .fetch_one(self.pool.as_ref())
        .await?;
        crate::BackfillState::try_from_row(&row)
    }

    /// Attempt to claim ownership of persisted runtime metadata backfill.
    ///
    /// Returns `true` when this runtime claimed the backfill worker slot.
    /// Returns `false` if backfill is already complete or currently owned by a
    /// non-expired worker.
    pub(crate) async fn try_claim_backfill(&self, lease_seconds: i64) -> anyhow::Result<bool> {
        self.ensure_backfill_state_row().await?;
        let now = jiff::Timestamp::now().as_second();
        let lease_cutoff = now.saturating_sub(lease_seconds.max(0));
        let result = sqlx::query(
            r#"
UPDATE backfill_state
SET status = ?, updated_at = ?
WHERE id = 1
  AND status != ?
  AND (status != ? OR updated_at <= ?)
            "#,
        )
        .bind(crate::BackfillStatus::Running.as_str())
        .bind(now)
        .bind(crate::BackfillStatus::Complete.as_str())
        .bind(crate::BackfillStatus::Running.as_str())
        .bind(lease_cutoff)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Mark persisted runtime metadata backfill as running.
    pub(crate) async fn mark_backfill_running(&self) -> anyhow::Result<()> {
        self.ensure_backfill_state_row().await?;
        let state = self.backfill().get_state().await?;
        if state.status == crate::BackfillStatus::Running {
            return Ok(());
        }
        let mut wf = BackfillWorkflow::from_status(state.status);
        anyhow::ensure!(
            wf.start(),
            "cannot transition backfill from {:?} to Running",
            state.status
        );

        sqlx::query(
            r#"
UPDATE backfill_state
SET status = ?, updated_at = ?
WHERE id = 1 AND status = ?
            "#,
        )
        .bind(crate::BackfillStatus::Running.as_str())
        .bind(jiff::Timestamp::now().as_second())
        .bind(state.status.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Persist runtime metadata backfill progress.
    pub(crate) async fn checkpoint_backfill(&self, watermark: &str) -> anyhow::Result<()> {
        self.ensure_backfill_state_row().await?;
        sqlx::query(
            r#"
UPDATE backfill_state
SET status = ?, last_watermark = ?, updated_at = ?
WHERE id = 1
            "#,
        )
        .bind(crate::BackfillStatus::Running.as_str())
        .bind(watermark)
        .bind(jiff::Timestamp::now().as_second())
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Mark runtime metadata backfill as complete.
    pub(crate) async fn mark_backfill_complete(
        &self,
        last_watermark: Option<&str>,
    ) -> anyhow::Result<()> {
        self.ensure_backfill_state_row().await?;
        let state = self.backfill().get_state().await?;
        let mut wf = BackfillWorkflow::from_status(state.status);
        anyhow::ensure!(
            wf.complete(),
            "cannot transition backfill from {:?} to Complete",
            state.status
        );

        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            r#"
UPDATE backfill_state
SET
    status = ?,
    last_watermark = COALESCE(?, last_watermark),
    last_success_at = ?,
    updated_at = ?
WHERE id = 1 AND status = ?
            "#,
        )
        .bind(crate::BackfillStatus::Complete.as_str())
        .bind(last_watermark)
        .bind(now)
        .bind(now)
        .bind(state.status.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    async fn ensure_backfill_state_row(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO backfill_state (id, status, last_watermark, last_success_at, updated_at)
VALUES (?, ?, NULL, NULL, ?)
ON CONFLICT(id) DO NOTHING
            "#,
        )
        .bind(1_i64)
        .bind(crate::BackfillStatus::Pending.as_str())
        .bind(jiff::Timestamp::now().as_second())
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
