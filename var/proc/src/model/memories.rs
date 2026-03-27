use anyhow::Result;
use chaos_ipc::ProcessId;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use std::path::PathBuf;

use super::ProcessMetadata;

/// Stored stage-1 memory extraction output for a single thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage1Output {
    pub process_id: ProcessId,
    pub rollout_path: PathBuf,
    pub source_updated_at: jiff::Timestamp,
    pub raw_memory: String,
    pub rollout_summary: String,
    pub rollout_slug: Option<String>,
    pub cwd: PathBuf,
    pub git_branch: Option<String>,
    pub generated_at: jiff::Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage1OutputRef {
    pub process_id: ProcessId,
    pub source_updated_at: jiff::Timestamp,
    pub rollout_slug: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Phase2InputSelection {
    pub selected: Vec<Stage1Output>,
    pub previous_selected: Vec<Stage1Output>,
    pub retained_process_ids: Vec<ProcessId>,
    pub removed: Vec<Stage1OutputRef>,
}

#[derive(Debug)]
pub(crate) struct Stage1OutputRow {
    process_id: String,
    rollout_path: String,
    source_updated_at: i64,
    raw_memory: String,
    rollout_summary: String,
    rollout_slug: Option<String>,
    cwd: String,
    git_branch: Option<String>,
    generated_at: i64,
}

impl Stage1OutputRow {
    pub(crate) fn try_from_row(row: &SqliteRow) -> Result<Self> {
        Ok(Self {
            process_id: row.try_get("process_id")?,
            rollout_path: row.try_get("rollout_path")?,
            source_updated_at: row.try_get("source_updated_at")?,
            raw_memory: row.try_get("raw_memory")?,
            rollout_summary: row.try_get("rollout_summary")?,
            rollout_slug: row.try_get("rollout_slug")?,
            cwd: row.try_get("cwd")?,
            git_branch: row.try_get("git_branch")?,
            generated_at: row.try_get("generated_at")?,
        })
    }
}

impl TryFrom<Stage1OutputRow> for Stage1Output {
    type Error = anyhow::Error;

    fn try_from(row: Stage1OutputRow) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            process_id: ProcessId::try_from(row.process_id)?,
            rollout_path: PathBuf::from(row.rollout_path),
            source_updated_at: epoch_seconds_to_datetime(row.source_updated_at)?,
            raw_memory: row.raw_memory,
            rollout_summary: row.rollout_summary,
            rollout_slug: row.rollout_slug,
            cwd: PathBuf::from(row.cwd),
            git_branch: row.git_branch,
            generated_at: epoch_seconds_to_datetime(row.generated_at)?,
        })
    }
}

fn epoch_seconds_to_datetime(secs: i64) -> Result<jiff::Timestamp> {
    jiff::Timestamp::from_second(secs)
        .map_err(|err| anyhow::anyhow!("invalid unix timestamp {secs}: {err}"))
}

pub(crate) fn stage1_output_ref_from_parts(
    process_id: String,
    source_updated_at: i64,
    rollout_slug: Option<String>,
) -> Result<Stage1OutputRef> {
    Ok(Stage1OutputRef {
        process_id: ProcessId::try_from(process_id)?,
        source_updated_at: epoch_seconds_to_datetime(source_updated_at)?,
        rollout_slug,
    })
}

/// Result of trying to claim a stage-1 memory extraction job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage1JobClaimOutcome {
    /// The caller owns the job and should continue with extraction.
    Claimed { ownership_token: String },
    /// Existing output is already newer than or equal to the source rollout.
    SkippedUpToDate,
    /// Another worker currently owns a fresh lease for this job.
    SkippedRunning,
    /// The job is in backoff and should not be retried yet.
    SkippedRetryBackoff,
    /// The job has exhausted retries and should not be retried automatically.
    SkippedRetryExhausted,
}

/// Claimed stage-1 job with thread metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage1JobClaim {
    pub thread: ProcessMetadata,
    pub ownership_token: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Stage1StartupClaimParams<'a> {
    pub scan_limit: usize,
    pub max_claimed: usize,
    pub max_age_days: i64,
    pub min_rollout_idle_hours: i64,
    pub allowed_sources: &'a [String],
    pub lease_seconds: i64,
}

/// Result of trying to claim a phase-2 consolidation job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase2JobClaimOutcome {
    /// The caller owns the global lock and should spawn consolidation.
    Claimed {
        ownership_token: String,
        /// Snapshot of `input_watermark` at claim time.
        input_watermark: i64,
    },
    /// The global job is not pending consolidation (or is already up to date).
    SkippedNotDirty,
    /// Another worker currently owns a fresh global consolidation lease.
    SkippedRunning,
}
