use super::{
    ChildAgentJob, ChildAgentJobCreateParams, ChildAgentJobItem, ChildAgentJobItemCreateParams,
    ChildAgentJobItemStatus, ChildAgentJobProgress, ChildAgentJobRow, ChildAgentJobStatus,
    QueryBuilder, Row, Sqlite, StateRuntime, Value,
};
use crate::model::ChildAgentJobItemRow;
use crate::model::child_agent_job_machine::item::ChildAgentJobItemWorkflow;
use crate::model::child_agent_job_machine::job::ChildAgentJobWorkflow;

impl StateRuntime {
    pub(crate) async fn create_child_agent_job(
        &self,
        params: &ChildAgentJobCreateParams,
        items: &[ChildAgentJobItemCreateParams],
    ) -> anyhow::Result<ChildAgentJob> {
        let now = jiff::Timestamp::now().as_second();
        let input_headers_json = serde_json::to_string(&params.input_headers)?;
        let output_schema_json = params
            .output_schema_json
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let max_runtime_seconds = params
            .max_runtime_seconds
            .map(i64::try_from)
            .transpose()
            .map_err(|_| anyhow::anyhow!("invalid max_runtime_seconds value"))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
INSERT INTO agent_jobs (
    id,
    name,
    status,
    instruction,
    auto_export,
    max_runtime_seconds,
    output_schema_json,
    input_headers_json,
    input_csv_path,
    output_csv_path,
    created_at,
    updated_at,
    started_at,
    completed_at,
    last_error
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL)
            "#,
        )
        .bind(params.id.as_str())
        .bind(params.name.as_str())
        .bind(ChildAgentJobStatus::Pending.as_str())
        .bind(params.instruction.as_str())
        .bind(i64::from(params.auto_export))
        .bind(max_runtime_seconds)
        .bind(output_schema_json)
        .bind(input_headers_json)
        .bind(params.input_csv_path.as_str())
        .bind(params.output_csv_path.as_str())
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        for item in items {
            let row_json = serde_json::to_string(&item.row_json)?;
            sqlx::query(
                r#"
INSERT INTO agent_job_items (
    job_id,
    item_id,
    row_index,
    source_id,
    row_json,
    status,
    assigned_process_id,
    attempt_count,
    result_json,
    last_error,
    created_at,
    updated_at,
    completed_at,
    reported_at
) VALUES (?, ?, ?, ?, ?, ?, NULL, 0, NULL, NULL, ?, ?, NULL, NULL)
                "#,
            )
            .bind(params.id.as_str())
            .bind(item.item_id.as_str())
            .bind(item.row_index)
            .bind(item.source_id.as_deref())
            .bind(row_json)
            .bind(ChildAgentJobItemStatus::Pending.as_str())
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        let job_id = params.id.as_str();
        self.get_child_agent_job(job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to load created child agent job {job_id}"))
    }

    pub(crate) async fn get_child_agent_job(
        &self,
        job_id: &str,
    ) -> anyhow::Result<Option<ChildAgentJob>> {
        let row = sqlx::query_as::<_, ChildAgentJobRow>(
            r#"
SELECT
    id,
    name,
    status,
    instruction,
    auto_export,
    max_runtime_seconds,
    output_schema_json,
    input_headers_json,
    input_csv_path,
    output_csv_path,
    created_at,
    updated_at,
    started_at,
    completed_at,
    last_error
FROM agent_jobs
WHERE id = ?
            "#,
        )
        .bind(job_id)
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(ChildAgentJob::try_from).transpose()
    }

    pub(crate) async fn list_child_agent_job_items(
        &self,
        job_id: &str,
        status: Option<ChildAgentJobItemStatus>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<ChildAgentJobItem>> {
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
SELECT
    job_id,
    item_id,
    row_index,
    source_id,
    row_json,
    status,
    assigned_process_id,
    attempt_count,
    result_json,
    last_error,
    created_at,
    updated_at,
    completed_at,
    reported_at
FROM agent_job_items
WHERE job_id = 
            "#,
        );
        builder.push_bind(job_id);
        if let Some(status) = status {
            builder.push(" AND status = ");
            builder.push_bind(status.as_str());
        }
        builder.push(" ORDER BY row_index ASC");
        if let Some(limit) = limit {
            builder.push(" LIMIT ");
            builder.push_bind(limit as i64);
        }
        let rows: Vec<ChildAgentJobItemRow> = builder
            .build_query_as::<ChildAgentJobItemRow>()
            .fetch_all(self.pool.as_ref())
            .await?;
        rows.into_iter().map(ChildAgentJobItem::try_from).collect()
    }

    pub(crate) async fn get_child_agent_job_item(
        &self,
        job_id: &str,
        item_id: &str,
    ) -> anyhow::Result<Option<ChildAgentJobItem>> {
        let row: Option<ChildAgentJobItemRow> = sqlx::query_as::<_, ChildAgentJobItemRow>(
            r#"
SELECT
    job_id,
    item_id,
    row_index,
    source_id,
    row_json,
    status,
    assigned_process_id,
    attempt_count,
    result_json,
    last_error,
    created_at,
    updated_at,
    completed_at,
    reported_at
FROM agent_job_items
WHERE job_id = ? AND item_id = ?
            "#,
        )
        .bind(job_id)
        .bind(item_id)
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(ChildAgentJobItem::try_from).transpose()
    }

    pub(crate) async fn mark_child_agent_job_running(&self, job_id: &str) -> anyhow::Result<()> {
        let status = self.get_child_agent_job_status(job_id).await?;
        let mut wf = ChildAgentJobWorkflow::from_status(status);
        anyhow::ensure!(
            wf.start(),
            "cannot transition job {job_id} from {status:?} to Running"
        );

        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            r#"
UPDATE agent_jobs
SET
    status = ?,
    updated_at = ?,
    started_at = COALESCE(started_at, ?),
    completed_at = NULL,
    last_error = NULL
WHERE id = ? AND status = ?
            "#,
        )
        .bind(ChildAgentJobStatus::Running.as_str())
        .bind(now)
        .bind(now)
        .bind(job_id)
        .bind(status.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub(crate) async fn mark_child_agent_job_completed(&self, job_id: &str) -> anyhow::Result<()> {
        let status = self.get_child_agent_job_status(job_id).await?;
        let mut wf = ChildAgentJobWorkflow::from_status(status);
        anyhow::ensure!(
            wf.complete(),
            "cannot transition job {job_id} from {status:?} to Completed"
        );

        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            r#"
UPDATE agent_jobs
SET status = ?, updated_at = ?, completed_at = ?, last_error = NULL
WHERE id = ? AND status = ?
            "#,
        )
        .bind(ChildAgentJobStatus::Completed.as_str())
        .bind(now)
        .bind(now)
        .bind(job_id)
        .bind(status.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub(crate) async fn mark_child_agent_job_failed(
        &self,
        job_id: &str,
        error_message: &str,
    ) -> anyhow::Result<()> {
        let status = self.get_child_agent_job_status(job_id).await?;
        let mut wf = ChildAgentJobWorkflow::from_status(status);
        anyhow::ensure!(
            wf.fail(),
            "cannot transition job {job_id} from {status:?} to Failed"
        );

        let now = jiff::Timestamp::now().as_second();
        sqlx::query(
            r#"
UPDATE agent_jobs
SET status = ?, updated_at = ?, completed_at = ?, last_error = ?
WHERE id = ? AND status = ?
            "#,
        )
        .bind(ChildAgentJobStatus::Failed.as_str())
        .bind(now)
        .bind(now)
        .bind(error_message)
        .bind(job_id)
        .bind(status.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub(crate) async fn mark_child_agent_job_cancelled(
        &self,
        job_id: &str,
        reason: &str,
    ) -> anyhow::Result<bool> {
        let status = self.get_child_agent_job_status(job_id).await?;
        let mut wf = ChildAgentJobWorkflow::from_status(status);
        if !wf.cancel() {
            return Ok(false);
        }

        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_jobs
SET status = ?, updated_at = ?, completed_at = ?, last_error = ?
WHERE id = ? AND status = ?
            "#,
        )
        .bind(ChildAgentJobStatus::Cancelled.as_str())
        .bind(now)
        .bind(now)
        .bind(reason)
        .bind(job_id)
        .bind(status.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_child_agent_job_status(
        &self,
        job_id: &str,
    ) -> anyhow::Result<ChildAgentJobStatus> {
        let row = sqlx::query(
            r#"
SELECT status FROM agent_jobs WHERE id = ?
            "#,
        )
        .bind(job_id)
        .fetch_optional(self.pool.as_ref())
        .await?;
        let row = row.ok_or_else(|| anyhow::anyhow!("child agent job {job_id} not found"))?;
        let status: String = row.try_get("status")?;
        ChildAgentJobStatus::parse(status.as_str())
    }

    pub(crate) async fn is_child_agent_job_cancelled(&self, job_id: &str) -> anyhow::Result<bool> {
        let row = sqlx::query(
            r#"
SELECT status
FROM agent_jobs
WHERE id = ?
            "#,
        )
        .bind(job_id)
        .fetch_optional(self.pool.as_ref())
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let status: String = row.try_get("status")?;
        Ok(ChildAgentJobStatus::parse(status.as_str())? == ChildAgentJobStatus::Cancelled)
    }

    pub async fn mark_child_agent_job_item_running(
        &self,
        job_id: &str,
        item_id: &str,
    ) -> anyhow::Result<bool> {
        // Validate: only Pending → Running is allowed.
        let mut wf = ChildAgentJobItemWorkflow::new();
        assert!(
            wf.start(),
            "item lifecycle: Pending → Running must be valid"
        );

        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    status = ?,
    assigned_process_id = NULL,
    attempt_count = attempt_count + 1,
    updated_at = ?,
    last_error = NULL
WHERE job_id = ? AND item_id = ? AND status = ?
            "#,
        )
        .bind(ChildAgentJobItemStatus::Running.as_str())
        .bind(now)
        .bind(job_id)
        .bind(item_id)
        .bind(ChildAgentJobItemStatus::Pending.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn mark_child_agent_job_item_running_with_thread(
        &self,
        job_id: &str,
        item_id: &str,
        process_id: &str,
    ) -> anyhow::Result<bool> {
        let mut wf = ChildAgentJobItemWorkflow::new();
        assert!(
            wf.start(),
            "item lifecycle: Pending → Running must be valid"
        );

        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    status = ?,
    assigned_process_id = ?,
    attempt_count = attempt_count + 1,
    updated_at = ?,
    last_error = NULL
WHERE job_id = ? AND item_id = ? AND status = ?
            "#,
        )
        .bind(ChildAgentJobItemStatus::Running.as_str())
        .bind(process_id)
        .bind(now)
        .bind(job_id)
        .bind(item_id)
        .bind(ChildAgentJobItemStatus::Pending.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn mark_child_agent_job_item_pending(
        &self,
        job_id: &str,
        item_id: &str,
        error_message: Option<&str>,
    ) -> anyhow::Result<bool> {
        // Validate: only Running → Pending (retry) is allowed.
        let mut wf = ChildAgentJobItemWorkflow::from_status(ChildAgentJobItemStatus::Running);
        assert!(
            wf.retry(),
            "item lifecycle: Running → Pending (retry) must be valid"
        );

        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    status = ?,
    assigned_process_id = NULL,
    updated_at = ?,
    last_error = ?
WHERE job_id = ? AND item_id = ? AND status = ?
            "#,
        )
        .bind(ChildAgentJobItemStatus::Pending.as_str())
        .bind(now)
        .bind(error_message)
        .bind(job_id)
        .bind(item_id)
        .bind(ChildAgentJobItemStatus::Running.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_child_agent_job_item_thread(
        &self,
        job_id: &str,
        item_id: &str,
        process_id: &str,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET assigned_process_id = ?, updated_at = ?
WHERE job_id = ? AND item_id = ? AND status = ?
            "#,
        )
        .bind(process_id)
        .bind(now)
        .bind(job_id)
        .bind(item_id)
        .bind(ChildAgentJobItemStatus::Running.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn report_child_agent_job_item_result(
        &self,
        job_id: &str,
        item_id: &str,
        reporting_process_id: &str,
        result_json: &Value,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let serialized = serde_json::to_string(result_json)?;
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    result_json = ?,
    reported_at = ?,
    updated_at = ?,
    last_error = NULL
WHERE
    job_id = ?
    AND item_id = ?
    AND status = ?
    AND assigned_process_id = ?
            "#,
        )
        .bind(serialized)
        .bind(now)
        .bind(now)
        .bind(job_id)
        .bind(item_id)
        .bind(ChildAgentJobItemStatus::Running.as_str())
        .bind(reporting_process_id)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn mark_child_agent_job_item_completed(
        &self,
        job_id: &str,
        item_id: &str,
    ) -> anyhow::Result<bool> {
        let mut wf = ChildAgentJobItemWorkflow::from_status(ChildAgentJobItemStatus::Running);
        assert!(
            wf.complete(),
            "item lifecycle: Running → Completed must be valid"
        );

        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    status = ?,
    completed_at = ?,
    updated_at = ?,
    assigned_process_id = NULL
WHERE
    job_id = ?
    AND item_id = ?
    AND status = ?
    AND result_json IS NOT NULL
            "#,
        )
        .bind(ChildAgentJobItemStatus::Completed.as_str())
        .bind(now)
        .bind(now)
        .bind(job_id)
        .bind(item_id)
        .bind(ChildAgentJobItemStatus::Running.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn mark_child_agent_job_item_failed(
        &self,
        job_id: &str,
        item_id: &str,
        error_message: &str,
    ) -> anyhow::Result<bool> {
        let mut wf = ChildAgentJobItemWorkflow::from_status(ChildAgentJobItemStatus::Running);
        assert!(wf.fail(), "item lifecycle: Running → Failed must be valid");

        let now = jiff::Timestamp::now().as_second();
        let result = sqlx::query(
            r#"
UPDATE agent_job_items
SET
    status = ?,
    completed_at = ?,
    updated_at = ?,
    last_error = ?,
    assigned_process_id = NULL
WHERE
    job_id = ?
    AND item_id = ?
    AND status = ?
            "#,
        )
        .bind(ChildAgentJobItemStatus::Failed.as_str())
        .bind(now)
        .bind(now)
        .bind(error_message)
        .bind(job_id)
        .bind(item_id)
        .bind(ChildAgentJobItemStatus::Running.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn get_child_agent_job_progress(
        &self,
        job_id: &str,
    ) -> anyhow::Result<ChildAgentJobProgress> {
        let row = sqlx::query(
            r#"
SELECT
    COUNT(*) AS total_items,
    SUM(CASE WHEN status = ? THEN 1 ELSE 0 END) AS pending_items,
    SUM(CASE WHEN status = ? THEN 1 ELSE 0 END) AS running_items,
    SUM(CASE WHEN status = ? THEN 1 ELSE 0 END) AS completed_items,
    SUM(CASE WHEN status = ? THEN 1 ELSE 0 END) AS failed_items
FROM agent_job_items
WHERE job_id = ?
            "#,
        )
        .bind(ChildAgentJobItemStatus::Pending.as_str())
        .bind(ChildAgentJobItemStatus::Running.as_str())
        .bind(ChildAgentJobItemStatus::Completed.as_str())
        .bind(ChildAgentJobItemStatus::Failed.as_str())
        .bind(job_id)
        .fetch_one(self.pool.as_ref())
        .await?;

        let total_items: i64 = row.try_get("total_items")?;
        let pending_items: Option<i64> = row.try_get("pending_items")?;
        let running_items: Option<i64> = row.try_get("running_items")?;
        let completed_items: Option<i64> = row.try_get("completed_items")?;
        let failed_items: Option<i64> = row.try_get("failed_items")?;
        Ok(ChildAgentJobProgress {
            total_items: usize::try_from(total_items).unwrap_or_default(),
            pending_items: usize::try_from(pending_items.unwrap_or_default()).unwrap_or_default(),
            running_items: usize::try_from(running_items.unwrap_or_default()).unwrap_or_default(),
            completed_items: usize::try_from(completed_items.unwrap_or_default())
                .unwrap_or_default(),
            failed_items: usize::try_from(failed_items.unwrap_or_default()).unwrap_or_default(),
        })
    }
}
