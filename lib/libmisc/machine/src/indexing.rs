//! Explicit, read-only indexing observations. No preferences or services change.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(any(target_os = "macos", test))]
mod macos;

/// A failed or unsupported probe is never a successful empty measurement.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IndexingObservation<T> {
    Available { value: T },
    Unavailable { reason: String },
    PermissionDenied,
    TimedOut,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotlightState {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexerProcess {
    pub pid: u32,
    /// Executable basename, not a command line or launchd service label.
    pub name: String,
    /// macOS `ps`'s decaying CPU average (up to a minute), not an instantaneous
    /// sample, per-workspace attribution, or indexing throughput.
    pub cpu_percent: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexingSnapshot {
    pub observed_at: SystemTime,
    pub workspace: PathBuf,
    /// Actual mount point resolved from the workspace, not an assumed `/`.
    pub volume: Option<PathBuf>,
    pub spotlight: IndexingObservation<SpotlightState>,
    /// Visible host processes, not just processes indexing this workspace.
    pub processes: IndexingObservation<Vec<IndexerProcess>>,
}

/// Inspect Spotlight for this existing absolute workspace and visible indexers.
///
/// macOS only; other platforms return `unsupported` without executing probes.
/// Each subprocess has a one-second deadline and bounded output. Filesystem
/// resolution is synchronous I/O; use a bounded blocking worker in async callers.
pub fn inspect_indexing(workspace: &Path) -> IndexingSnapshot {
    let snapshot = IndexingSnapshot {
        observed_at: SystemTime::now(),
        workspace: workspace.to_path_buf(),
        volume: None,
        spotlight: IndexingObservation::Unsupported,
        processes: IndexingObservation::Unsupported,
    };
    #[cfg(target_os = "macos")]
    {
        macos::inspect(snapshot)
    }
    #[cfg(not(target_os = "macos"))]
    snapshot
}

#[cfg(test)]
mod tests;
