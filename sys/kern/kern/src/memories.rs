//! Memory subsystem — extraction and consolidation of session memories.
//!
//! The startup memory pipeline runs two stages:
//! - **Extraction**: select rollouts, run stage-1 LLM extraction to produce raw memories.
//! - **Consolidation**: merge/deduplicate raw memories via a sub-agent.

pub(crate) mod citations;
mod consolidation;
mod control;
mod extraction;
mod pipeline;
pub(crate) mod prompts;
mod storage;
pub(crate) mod usage;

use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ReasoningEffort;

pub(crate) use control::clear_memory_root_contents;
/// Starts the memory startup pipeline for eligible root sessions.
/// This is the single entrypoint that `codex` uses to trigger memory startup.
///
/// This is the entry point to read and understand this module.
pub(crate) use pipeline::start_memories_startup_task;

/// Returns the reasoning effort to use for a memory pipeline stage, gated on
/// whether the chosen model actually supports reasoning.
///
/// When `model_info.supported_reasoning_levels` is empty the model does not
/// support reasoning and the parameter must be omitted entirely.
pub(in crate::memories) fn reasoning_effort_for_model(
    model_info: &ModelInfo,
    default: ReasoningEffort,
) -> Option<ReasoningEffort> {
    if model_info.supported_reasoning_levels.is_empty() {
        None
    } else {
        Some(default)
    }
}

/// Phase 1 (startup extraction).
mod phase_one {
    /// Default reasoning effort used for phase 1.
    pub(super) const REASONING_EFFORT: super::ReasoningEffort = super::ReasoningEffort::Low;
    /// Prompt used for phase 1.
    pub(super) const PROMPT: &str = include_str!("../templates/memories/stage_one_system.md");
    /// Concurrency cap for startup memory extraction and consolidation scheduling.
    pub(super) const CONCURRENCY_LIMIT: usize = 8;
    /// Lease duration (seconds) for phase-1 job ownership.
    pub(super) const JOB_LEASE_SECONDS: i64 = 3_600;
    /// Backoff delay (seconds) before retrying a failed stage-1 extraction job.
    pub(super) const JOB_RETRY_DELAY_SECONDS: i64 = 3_600;
    /// Maximum number of threads to scan.
    pub(super) const THREAD_SCAN_LIMIT: usize = 5_000;
    /// Size of the batches when pruning old thread memories.
    pub(super) const PRUNE_BATCH_SIZE: usize = 200;
}

/// Phase 2 (aka `Consolidation`).
mod phase_two {
    /// Default reasoning effort used for phase 2.
    pub(super) const REASONING_EFFORT: super::ReasoningEffort = super::ReasoningEffort::Medium;
    /// Lease duration (seconds) for phase-2 consolidation job ownership.
    pub(super) const JOB_LEASE_SECONDS: i64 = 3_600;
    /// Backoff delay (seconds) before retrying a failed phase-2 consolidation
    /// job.
    pub(super) const JOB_RETRY_DELAY_SECONDS: i64 = 3_600;
    /// Heartbeat interval (seconds) for phase-2 running jobs.
    pub(super) const JOB_HEARTBEAT_SECONDS: u64 = 90;
}

mod metrics {
    /// Number of phase-1 startup jobs grouped by status.
    pub(super) const MEMORY_PHASE_ONE_JOBS: &str = "codex.memory.phase1";
    /// End-to-end latency for a single phase-1 startup run.
    pub(super) const MEMORY_PHASE_ONE_E2E_MS: &str = "codex.memory.phase1.e2e_ms";
    /// Number of raw memories produced by phase-1 startup extraction.
    pub(super) const MEMORY_PHASE_ONE_OUTPUT: &str = "codex.memory.phase1.output";
    /// Histogram for aggregate token usage across one phase-1 startup run.
    pub(super) const MEMORY_PHASE_ONE_TOKEN_USAGE: &str = "codex.memory.phase1.token_usage";
    /// Number of phase-2 startup jobs grouped by status.
    pub(super) const MEMORY_PHASE_TWO_JOBS: &str = "codex.memory.phase2";
    /// End-to-end latency for a single phase-2 consolidation run.
    pub(super) const MEMORY_PHASE_TWO_E2E_MS: &str = "codex.memory.phase2.e2e_ms";
    /// Number of stage-1 memories included in each phase-2 consolidation step.
    pub(super) const MEMORY_PHASE_TWO_INPUT: &str = "codex.memory.phase2.input";
    /// Histogram for aggregate token usage across one phase-2 consolidation run.
    pub(super) const MEMORY_PHASE_TWO_TOKEN_USAGE: &str = "codex.memory.phase2.token_usage";
}

use std::path::Path;
use std::path::PathBuf;

pub fn memory_root(chaos_home: &Path) -> PathBuf {
    chaos_home.join("memories")
}
