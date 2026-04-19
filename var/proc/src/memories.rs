//! Memory pipeline API surface for runtime state.
//!
//! This module keeps stage-1/phase-2 memory types and operations under an
//! explicit namespace instead of flattening them into the crate root or the
//! general `StateRuntime` facade.

use crate::model::Phase2InputSelection;
use crate::model::Phase2JobClaimOutcome;
use crate::model::Stage1JobClaim;
use crate::model::Stage1JobClaimOutcome;
use crate::model::Stage1Output;
use crate::model::Stage1StartupClaimParams;
use crate::runtime::StateRuntime;
use chaos_ipc::ProcessId;

pub use crate::model::Phase2InputSelection as InputSelection;
pub use crate::model::Phase2JobClaimOutcome as GlobalClaimOutcome;
pub use crate::model::Stage1JobClaim as Claim;
pub use crate::model::Stage1JobClaimOutcome as ClaimOutcome;
pub use crate::model::Stage1Output as Output;
pub use crate::model::Stage1OutputRef as OutputRef;
pub use crate::model::Stage1StartupClaimParams as StartupClaimParams;

#[derive(Clone, Copy)]
pub struct MemoryRuntime<'a> {
    runtime: &'a StateRuntime,
}

impl StateRuntime {
    pub fn memories(&self) -> MemoryRuntime<'_> {
        MemoryRuntime { runtime: self }
    }
}

impl<'a> MemoryRuntime<'a> {
    pub async fn claim_stage1_jobs_for_startup(
        &self,
        current_process_id: ProcessId,
        params: Stage1StartupClaimParams<'_>,
    ) -> anyhow::Result<Vec<Stage1JobClaim>> {
        self.runtime
            .claim_stage1_jobs_for_startup(current_process_id, params)
            .await
    }

    pub async fn try_claim_stage1_job(
        &self,
        process_id: ProcessId,
        worker_id: ProcessId,
        source_updated_at: i64,
        lease_seconds: i64,
        max_running_jobs: usize,
    ) -> anyhow::Result<Stage1JobClaimOutcome> {
        self.runtime
            .try_claim_stage1_job(
                process_id,
                worker_id,
                source_updated_at,
                lease_seconds,
                max_running_jobs,
            )
            .await
    }

    pub async fn mark_stage1_job_succeeded(
        &self,
        process_id: ProcessId,
        ownership_token: &str,
        source_updated_at: i64,
        raw_memory: &str,
        rollout_summary: &str,
        rollout_slug: Option<&str>,
    ) -> anyhow::Result<bool> {
        self.runtime
            .mark_stage1_job_succeeded(
                process_id,
                ownership_token,
                source_updated_at,
                raw_memory,
                rollout_summary,
                rollout_slug,
            )
            .await
    }

    pub async fn mark_stage1_job_succeeded_no_output(
        &self,
        process_id: ProcessId,
        ownership_token: &str,
    ) -> anyhow::Result<bool> {
        self.runtime
            .mark_stage1_job_succeeded_no_output(process_id, ownership_token)
            .await
    }

    pub async fn mark_stage1_job_failed(
        &self,
        process_id: ProcessId,
        ownership_token: &str,
        failure_reason: &str,
        retry_delay_seconds: i64,
    ) -> anyhow::Result<bool> {
        self.runtime
            .mark_stage1_job_failed(
                process_id,
                ownership_token,
                failure_reason,
                retry_delay_seconds,
            )
            .await
    }

    pub async fn enqueue_global_consolidation(&self, input_watermark: i64) -> anyhow::Result<()> {
        self.runtime
            .enqueue_global_consolidation(input_watermark)
            .await
    }

    pub async fn try_claim_global_phase2_job(
        &self,
        worker_id: ProcessId,
        lease_seconds: i64,
    ) -> anyhow::Result<Phase2JobClaimOutcome> {
        self.runtime
            .try_claim_global_phase2_job(worker_id, lease_seconds)
            .await
    }

    pub async fn heartbeat_global_phase2_job(
        &self,
        ownership_token: &str,
        lease_seconds: i64,
    ) -> anyhow::Result<bool> {
        self.runtime
            .heartbeat_global_phase2_job(ownership_token, lease_seconds)
            .await
    }

    pub async fn mark_global_phase2_job_succeeded(
        &self,
        ownership_token: &str,
        completed_watermark: i64,
        selected_outputs: &[Stage1Output],
    ) -> anyhow::Result<bool> {
        self.runtime
            .mark_global_phase2_job_succeeded(
                ownership_token,
                completed_watermark,
                selected_outputs,
            )
            .await
    }

    pub async fn mark_global_phase2_job_failed(
        &self,
        ownership_token: &str,
        failure_reason: &str,
        retry_delay_seconds: i64,
    ) -> anyhow::Result<bool> {
        self.runtime
            .mark_global_phase2_job_failed(ownership_token, failure_reason, retry_delay_seconds)
            .await
    }

    pub async fn mark_global_phase2_job_failed_if_unowned(
        &self,
        ownership_token: &str,
        failure_reason: &str,
        retry_delay_seconds: i64,
    ) -> anyhow::Result<bool> {
        self.runtime
            .mark_global_phase2_job_failed_if_unowned(
                ownership_token,
                failure_reason,
                retry_delay_seconds,
            )
            .await
    }

    pub async fn clear_memory_data(&self) -> anyhow::Result<()> {
        self.runtime.clear_memory_data().await
    }

    pub async fn reset_memory_data_for_fresh_start(&self) -> anyhow::Result<()> {
        self.runtime.reset_memory_data_for_fresh_start().await
    }

    pub async fn mark_process_memory_mode_polluted(
        &self,
        process_id: ProcessId,
    ) -> anyhow::Result<bool> {
        self.runtime
            .mark_process_memory_mode_polluted(process_id)
            .await
    }

    pub async fn record_stage1_output_usage(
        &self,
        process_ids: &[ProcessId],
    ) -> anyhow::Result<usize> {
        self.runtime.record_stage1_output_usage(process_ids).await
    }

    pub async fn list_stage1_outputs_for_global(
        &self,
        n: usize,
    ) -> anyhow::Result<Vec<Stage1Output>> {
        self.runtime.list_stage1_outputs_for_global(n).await
    }

    pub async fn prune_stage1_outputs_for_retention(
        &self,
        max_unused_days: i64,
        limit: usize,
    ) -> anyhow::Result<usize> {
        self.runtime
            .prune_stage1_outputs_for_retention(max_unused_days, limit)
            .await
    }

    pub async fn get_phase2_input_selection(
        &self,
        n: usize,
        max_unused_days: i64,
    ) -> anyhow::Result<Phase2InputSelection> {
        self.runtime
            .get_phase2_input_selection(n, max_unused_days)
            .await
    }
}
