//! Child agent job API surface for runtime state.

use crate::ChildAgentJob;
use crate::ChildAgentJobCreateParams;
use crate::ChildAgentJobItem;
use crate::ChildAgentJobItemCreateParams;
use crate::ChildAgentJobItemStatus;
use crate::ChildAgentJobProgress;
use crate::RuntimeDbHandle;
use serde_json::Value;

#[derive(Clone, Copy)]
pub struct ChildAgentJobs<'a> {
    db: &'a RuntimeDbHandle,
}

impl RuntimeDbHandle {
    pub fn child_agent_jobs(&self) -> ChildAgentJobs<'_> {
        ChildAgentJobs { db: self }
    }
}

impl<'a> ChildAgentJobs<'a> {
    pub async fn create(
        &self,
        params: &ChildAgentJobCreateParams,
        items: &[ChildAgentJobItemCreateParams],
    ) -> anyhow::Result<ChildAgentJob> {
        self.db.create_child_agent_job(params, items).await
    }

    pub async fn get(&self, job_id: &str) -> anyhow::Result<Option<ChildAgentJob>> {
        self.db.get_child_agent_job(job_id).await
    }

    pub async fn list_items(
        &self,
        job_id: &str,
        status: Option<ChildAgentJobItemStatus>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<ChildAgentJobItem>> {
        self.db
            .list_child_agent_job_items(job_id, status, limit)
            .await
    }

    pub async fn get_item(
        &self,
        job_id: &str,
        item_id: &str,
    ) -> anyhow::Result<Option<ChildAgentJobItem>> {
        self.db.get_child_agent_job_item(job_id, item_id).await
    }

    pub async fn mark_running(&self, job_id: &str) -> anyhow::Result<()> {
        self.db.mark_child_agent_job_running(job_id).await
    }

    pub async fn mark_completed(&self, job_id: &str) -> anyhow::Result<()> {
        self.db.mark_child_agent_job_completed(job_id).await
    }

    pub async fn mark_failed(&self, job_id: &str, error_message: &str) -> anyhow::Result<()> {
        self.db
            .mark_child_agent_job_failed(job_id, error_message)
            .await
    }

    pub async fn mark_cancelled(&self, job_id: &str, reason: &str) -> anyhow::Result<bool> {
        self.db.mark_child_agent_job_cancelled(job_id, reason).await
    }

    pub async fn is_cancelled(&self, job_id: &str) -> anyhow::Result<bool> {
        self.db.is_child_agent_job_cancelled(job_id).await
    }

    pub async fn mark_item_running_with_thread(
        &self,
        job_id: &str,
        item_id: &str,
        process_id: &str,
    ) -> anyhow::Result<bool> {
        self.db
            .mark_child_agent_job_item_running_with_thread(job_id, item_id, process_id)
            .await
    }

    pub async fn mark_item_pending(
        &self,
        job_id: &str,
        item_id: &str,
        error_message: Option<&str>,
    ) -> anyhow::Result<bool> {
        self.db
            .mark_child_agent_job_item_pending(job_id, item_id, error_message)
            .await
    }

    pub async fn report_item_result(
        &self,
        job_id: &str,
        item_id: &str,
        reporting_process_id: &str,
        result_json: &Value,
    ) -> anyhow::Result<bool> {
        self.db
            .report_child_agent_job_item_result(job_id, item_id, reporting_process_id, result_json)
            .await
    }

    pub async fn mark_item_completed(&self, job_id: &str, item_id: &str) -> anyhow::Result<bool> {
        self.db
            .mark_child_agent_job_item_completed(job_id, item_id)
            .await
    }

    pub async fn mark_item_failed(
        &self,
        job_id: &str,
        item_id: &str,
        error_message: &str,
    ) -> anyhow::Result<bool> {
        self.db
            .mark_child_agent_job_item_failed(job_id, item_id, error_message)
            .await
    }

    pub async fn progress(&self, job_id: &str) -> anyhow::Result<ChildAgentJobProgress> {
        self.db.get_child_agent_job_progress(job_id).await
    }
}
