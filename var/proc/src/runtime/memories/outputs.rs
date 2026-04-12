use super::super::StateRuntime;
use super::whole_days_as_hours;
use crate::model::{
    Phase2InputSelection, Stage1Output, Stage1OutputRow, stage1_output_ref_from_parts,
};
use chaos_ipc::ProcessId;
use sqlx::Row;
use std::collections::HashSet;

impl StateRuntime {
    /// Record usage for cited stage-1 outputs.
    ///
    /// Each thread id increments `usage_count` by one and sets `last_usage` to
    /// the current Unix timestamp. Missing rows are ignored.
    pub async fn record_stage1_output_usage(
        &self,
        process_ids: &[ProcessId],
    ) -> anyhow::Result<usize> {
        if process_ids.is_empty() {
            return Ok(0);
        }

        let now = jiff::Timestamp::now().as_second();
        let mut tx = self.pool.begin().await?;
        let mut updated_rows = 0;

        for process_id in process_ids {
            updated_rows += sqlx::query(
                r#"
UPDATE stage1_outputs
SET
    usage_count = COALESCE(usage_count, 0) + 1,
    last_usage = ?
WHERE process_id = ?
                "#,
            )
            .bind(now)
            .bind(process_id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;
        }

        tx.commit().await?;
        Ok(updated_rows)
    }

    /// Lists the most recent non-empty stage-1 outputs for global consolidation.
    ///
    /// Query behavior:
    /// - filters out rows where both `raw_memory` and `rollout_summary` are blank
    /// - joins `processes` to include thread `cwd` and `git_branch`
    /// - orders by `source_updated_at DESC, process_id DESC`
    /// - applies `LIMIT n`
    pub async fn list_stage1_outputs_for_global(
        &self,
        n: usize,
    ) -> anyhow::Result<Vec<Stage1Output>> {
        if n == 0 {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            r#"
SELECT
    so.process_id,
    so.process_id AS process_ref,
    so.source_updated_at,
    so.raw_memory,
    so.rollout_summary,
    so.rollout_slug,
    so.generated_at,
    COALESCE(t.cwd, '') AS cwd,
    t.git_branch AS git_branch
FROM stage1_outputs AS so
LEFT JOIN processes AS t
    ON t.id = so.process_id
WHERE t.memory_mode = 'enabled'
  AND (length(trim(so.raw_memory)) > 0 OR length(trim(so.rollout_summary)) > 0)
ORDER BY so.source_updated_at DESC, so.process_id DESC
LIMIT ?
            "#,
        )
        .bind(n as i64)
        .fetch_all(self.pool.as_ref())
        .await?;

        rows.into_iter()
            .map(|row| Stage1OutputRow::try_from_row(&row).and_then(Stage1Output::try_from))
            .collect::<Result<Vec<_>, _>>()
    }

    /// Prunes stale stage-1 outputs while preserving the latest phase-2
    /// baseline and stage-1 job watermarks.
    ///
    /// Query behavior:
    /// - considers only rows with `selected_for_phase2 = 0`
    /// - keeps recency as `COALESCE(last_usage, source_updated_at)`
    /// - removes rows older than `max_unused_days`
    /// - prunes at most `limit` rows ordered from stalest to newest
    pub async fn prune_stage1_outputs_for_retention(
        &self,
        max_unused_days: i64,
        limit: usize,
    ) -> anyhow::Result<usize> {
        if limit == 0 {
            return Ok(0);
        }

        let cutoff = jiff::Timestamp::now()
            .checked_sub(whole_days_as_hours(max_unused_days.max(0)))
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
            .as_second();
        let rows_affected = sqlx::query(
            r#"
DELETE FROM stage1_outputs
WHERE process_id IN (
    SELECT process_id
    FROM stage1_outputs
    WHERE selected_for_phase2 = 0
      AND COALESCE(last_usage, source_updated_at) < ?
    ORDER BY
      COALESCE(last_usage, source_updated_at) ASC,
      source_updated_at ASC,
      process_id ASC
    LIMIT ?
)
            "#,
        )
        .bind(cutoff)
        .bind(limit as i64)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();

        Ok(rows_affected as usize)
    }

    /// Returns the current phase-2 input set along with its diff against the
    /// last successful phase-2 selection.
    ///
    /// Query behavior:
    /// - current selection keeps only non-empty stage-1 outputs whose
    ///   `last_usage` is within `max_unused_days`, or whose
    ///   `source_updated_at` is within that window when the memory has never
    ///   been used
    /// - eligible rows are ordered by `usage_count DESC`,
    ///   `COALESCE(last_usage, source_updated_at) DESC`, `source_updated_at DESC`,
    ///   `process_id DESC`
    /// - previously selected rows are identified by `selected_for_phase2 = 1`
    /// - `previous_selected` contains the current persisted rows that belonged
    ///   to the last successful phase-2 baseline, even if those processes are no
    ///   longer memory-eligible
    /// - `retained_process_ids` records which current rows still match the exact
    ///   snapshot selected in the last successful phase-2 run
    /// - removed rows are previously selected rows that are still present in
    ///   `stage1_outputs` but are no longer in the current selection, including
    ///   processes that are no longer memory-eligible
    pub async fn get_phase2_input_selection(
        &self,
        n: usize,
        max_unused_days: i64,
    ) -> anyhow::Result<Phase2InputSelection> {
        if n == 0 {
            return Ok(Phase2InputSelection::default());
        }
        let cutoff = jiff::Timestamp::now()
            .checked_sub(whole_days_as_hours(max_unused_days.max(0)))
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
            .as_second();

        let current_rows = sqlx::query(
            r#"
SELECT
    so.process_id,
    so.process_id AS process_ref,
    so.source_updated_at,
    so.raw_memory,
    so.rollout_summary,
    so.rollout_slug,
    so.generated_at,
    COALESCE(t.cwd, '') AS cwd,
    t.git_branch AS git_branch,
    so.selected_for_phase2,
    so.selected_for_phase2_source_updated_at
FROM stage1_outputs AS so
LEFT JOIN processes AS t
    ON t.id = so.process_id
WHERE t.memory_mode = 'enabled'
  AND (length(trim(so.raw_memory)) > 0 OR length(trim(so.rollout_summary)) > 0)
  AND (
        (so.last_usage IS NOT NULL AND so.last_usage >= ?)
        OR (so.last_usage IS NULL AND so.source_updated_at >= ?)
  )
ORDER BY
    COALESCE(so.usage_count, 0) DESC,
    COALESCE(so.last_usage, so.source_updated_at) DESC,
    so.source_updated_at DESC,
    so.process_id DESC
LIMIT ?
            "#,
        )
        .bind(cutoff)
        .bind(cutoff)
        .bind(n as i64)
        .fetch_all(self.pool.as_ref())
        .await?;

        let mut current_process_ids = HashSet::with_capacity(current_rows.len());
        let mut selected = Vec::with_capacity(current_rows.len());
        let mut retained_process_ids = Vec::new();
        for row in current_rows {
            let process_id = row.try_get::<String, _>("process_id")?;
            current_process_ids.insert(process_id.clone());
            let source_updated_at = row.try_get::<i64, _>("source_updated_at")?;
            if row.try_get::<i64, _>("selected_for_phase2")? != 0
                && row.try_get::<Option<i64>, _>("selected_for_phase2_source_updated_at")?
                    == Some(source_updated_at)
            {
                retained_process_ids.push(ProcessId::try_from(process_id.clone())?);
            }
            selected.push(Stage1Output::try_from(Stage1OutputRow::try_from_row(
                &row,
            )?)?);
        }

        let previous_rows = sqlx::query(
            r#"
SELECT
    so.process_id,
    so.process_id AS process_ref,
    so.source_updated_at,
    so.raw_memory,
    so.rollout_summary,
    so.rollout_slug,
    so.generated_at,
    COALESCE(t.cwd, '') AS cwd,
    t.git_branch AS git_branch
FROM stage1_outputs AS so
LEFT JOIN processes AS t
    ON t.id = so.process_id
WHERE so.selected_for_phase2 = 1
ORDER BY so.source_updated_at DESC, so.process_id DESC
            "#,
        )
        .fetch_all(self.pool.as_ref())
        .await?;

        let previous_selected = previous_rows
            .iter()
            .map(Stage1OutputRow::try_from_row)
            .map(|row| row.and_then(Stage1Output::try_from))
            .collect::<Result<Vec<_>, _>>()?;
        let mut removed = Vec::new();
        for row in previous_rows {
            let process_id = row.try_get::<String, _>("process_id")?;
            if current_process_ids.contains(process_id.as_str()) {
                continue;
            }
            removed.push(stage1_output_ref_from_parts(
                process_id,
                row.try_get("source_updated_at")?,
                row.try_get("rollout_slug")?,
            )?);
        }

        Ok(Phase2InputSelection {
            selected,
            previous_selected,
            retained_process_ids,
            removed,
        })
    }
}
