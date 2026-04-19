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
mod tests {
    use super::StateRuntime;
    use crate::runtime::test_support::unique_temp_dir;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn init_creates_runtime_db() {
        let chaos_home = unique_temp_dir();
        tokio::fs::create_dir_all(&chaos_home)
            .await
            .expect("create chaos_home");

        let _runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        assert_eq!(
            tokio::fs::try_exists(chaos_home.join(crate::runtime::runtime_db_filename()))
                .await
                .expect("check new db path"),
            true
        );

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn backfill_state_persists_progress_and_completion() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let initial = runtime
            .backfill()
            .get_state()
            .await
            .expect("get initial backfill state");
        assert_eq!(initial.status, crate::BackfillStatus::Pending);
        assert_eq!(initial.last_watermark, None);
        assert_eq!(initial.last_success_at, None);

        runtime
            .backfill()
            .mark_running()
            .await
            .expect("mark backfill running");
        runtime
            .backfill()
            .checkpoint("cursor-a")
            .await
            .expect("checkpoint backfill");

        let running = runtime
            .backfill()
            .get_state()
            .await
            .expect("get running backfill state");
        assert_eq!(running.status, crate::BackfillStatus::Running);
        assert_eq!(running.last_watermark, Some("cursor-a".to_string()));
        assert_eq!(running.last_success_at, None);

        runtime
            .backfill()
            .mark_complete(Some("cursor-b"))
            .await
            .expect("mark backfill complete");
        let completed = runtime
            .backfill()
            .get_state()
            .await
            .expect("get completed backfill state");
        assert_eq!(completed.status, crate::BackfillStatus::Complete);
        assert_eq!(completed.last_watermark, Some("cursor-b".to_string()));
        assert!(completed.last_success_at.is_some());

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn backfill_claim_is_singleton_until_stale_and_blocked_when_complete() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let claimed = runtime
            .backfill()
            .try_claim(3600)
            .await
            .expect("initial backfill claim");
        assert_eq!(claimed, true);

        let duplicate_claim = runtime
            .backfill()
            .try_claim(3600)
            .await
            .expect("duplicate backfill claim");
        assert_eq!(duplicate_claim, false);

        let stale_updated_at = jiff::Timestamp::now().as_second().saturating_sub(10_000);
        sqlx::query(
            r#"
UPDATE backfill_state
SET status = ?, updated_at = ?
WHERE id = 1
            "#,
        )
        .bind(crate::BackfillStatus::Running.as_str())
        .bind(stale_updated_at)
        .execute(runtime.pool.as_ref())
        .await
        .expect("force stale backfill lease");

        let stale_claim = runtime
            .backfill()
            .try_claim(10)
            .await
            .expect("stale backfill claim");
        assert_eq!(stale_claim, true);

        runtime
            .backfill()
            .mark_complete(None)
            .await
            .expect("mark complete");
        let claim_after_complete = runtime
            .backfill()
            .try_claim(3600)
            .await
            .expect("claim after complete");
        assert_eq!(claim_after_complete, false);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn mark_backfill_running_is_idempotent_after_claim() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let claimed = runtime
            .backfill()
            .try_claim(3600)
            .await
            .expect("claim backfill");
        assert_eq!(claimed, true);

        runtime
            .backfill()
            .mark_running()
            .await
            .expect("mark running after claim");

        let state = runtime
            .backfill()
            .get_state()
            .await
            .expect("get backfill state after claim");
        assert_eq!(state.status, crate::BackfillStatus::Running);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }
}
