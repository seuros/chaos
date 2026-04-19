use super::super::StateRuntime;
use super::{
    JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL, JOB_KIND_MEMORY_STAGE1,
    enqueue_global_consolidation_with_executor,
};
use chaos_ipc::ProcessId;

impl StateRuntime {
    /// Deletes all persisted memory state in one transaction.
    ///
    /// This removes every `stage1_outputs` row and all `jobs` rows for the
    /// stage-1 (`memory_stage1`) and phase-2 (`memory_consolidate_global`)
    /// memory pipelines.
    pub(crate) async fn clear_memory_data(&self) -> anyhow::Result<()> {
        self.clear_memory_data_inner(/*disable_existing_threads*/ false)
            .await
    }

    /// Resets persisted memory state for a clean-slate local start.
    ///
    /// In addition to clearing persisted stage-1 outputs and memory pipeline
    /// jobs, this disables memory generation for all existing processes so
    /// historical rollouts are not immediately picked up again.
    pub(crate) async fn reset_memory_data_for_fresh_start(&self) -> anyhow::Result<()> {
        self.clear_memory_data_inner(/*disable_existing_threads*/ true)
            .await
    }

    pub(super) async fn clear_memory_data_inner(
        &self,
        disable_existing_threads: bool,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
DELETE FROM stage1_outputs
            "#,
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
DELETE FROM jobs
WHERE kind = ? OR kind = ?
            "#,
        )
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .execute(&mut *tx)
        .await?;

        if disable_existing_threads {
            sqlx::query(
                r#"
UPDATE processes
SET memory_mode = 'disabled'
WHERE memory_mode = 'enabled'
                "#,
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    /// Marks a thread as polluted and enqueues phase-2 forgetting when the
    /// thread participated in the last successful phase-2 baseline.
    pub(crate) async fn mark_process_memory_mode_polluted(
        &self,
        process_id: ProcessId,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let process_id = process_id.to_string();
        let mut tx = self.pool.begin().await?;
        let rows_affected = sqlx::query(
            r#"
UPDATE processes
SET memory_mode = 'polluted'
WHERE id = ? AND memory_mode != 'polluted'
            "#,
        )
        .bind(process_id.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if rows_affected == 0 {
            tx.commit().await?;
            return Ok(false);
        }

        let selected_for_phase2 = sqlx::query_scalar::<_, i64>(
            r#"
SELECT selected_for_phase2
FROM stage1_outputs
WHERE process_id = ?
            "#,
        )
        .bind(process_id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(0);
        if selected_for_phase2 != 0 {
            enqueue_global_consolidation_with_executor(&mut *tx, now).await?;
        }

        tx.commit().await?;
        Ok(true)
    }
}
