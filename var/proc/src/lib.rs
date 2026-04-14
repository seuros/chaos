//! Runtime data for persisted session metadata.
//!
//! This crate owns the local process metadata, memories, and log indexes derived
//! from persisted session history. Session replay itself lives in journald.
//! SQLite remains the primary runtime implementation today; Postgres bootstrap
//! helpers are exposed for backend-aware consumers.

mod extract;
pub mod log_db;
mod migrations;
mod model;
mod runtime;

pub use model::LogEntry;
pub use model::LogQuery;
pub use model::LogRow;
pub use model::LogTailBatch;
pub use model::LogTailCursor;
pub use model::Phase2InputSelection;
pub use model::Phase2JobClaimOutcome;
/// Preferred entrypoint: owns configuration and metrics.
pub use runtime::RuntimeDbHandle;
pub use runtime::StateRuntime;

/// Low-level storage engine: useful for focused tests.
///
/// Most consumers should prefer [`StateRuntime`].
pub use extract::apply_rollout_item;
pub use extract::rollout_item_affects_process_metadata;
pub use model::AgentJob;
pub use model::AgentJobCreateParams;
pub use model::AgentJobItem;
pub use model::AgentJobItemCreateParams;
pub use model::AgentJobItemStatus;
pub use model::AgentJobProgress;
pub use model::AgentJobStatus;
pub use model::Anchor;
pub use model::BackfillState;
pub use model::BackfillStats;
pub use model::BackfillStatus;
pub use model::ExtractionOutcome;
pub use model::ProcessMetadata;
pub use model::ProcessMetadataBuilder;
pub use model::ProcessesPage;
pub use model::SortKey;
pub use model::Stage1JobClaim;
pub use model::Stage1JobClaimOutcome;
pub use model::Stage1Output;
pub use model::Stage1OutputRef;
pub use model::Stage1StartupClaimParams;
pub use runtime::open_runtime_db;
pub use runtime::open_runtime_db_at_path;
pub use runtime::open_runtime_db_postgres_url;
pub use runtime::open_runtime_db_url;
pub use runtime::runtime_db_filename;
pub use runtime::runtime_db_path;

/// Environment variable for overriding the SQLite runtime database home directory.
pub const SQLITE_HOME_ENV: &str = "CHAOS_SQLITE_HOME";

pub fn sqlite_home_env_value() -> Option<String> {
    if let Ok(value) = std::env::var(SQLITE_HOME_ENV) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}
