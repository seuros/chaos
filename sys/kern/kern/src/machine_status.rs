//! Bounded host observations shared by model checks, resources, and terminal chrome.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chaos_machine::{MachineSnapshot, StorageRole, StorageSnapshot, StorageTarget};
use serde::Serialize;

use crate::config::{Config, MachineWarningsConfig};
use crate::machine_warnings;
pub use crate::machine_warnings::MachineWarning;

#[derive(Debug, Clone, Serialize)]
pub struct MachineStatus {
    pub machine: MachineSnapshot,
    pub storage: StorageSnapshot,
    /// Collected only for explicit resource reads, not UI or warning polling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing: Option<chaos_machine::IndexingSnapshot>,
    pub warnings: Vec<MachineWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_instruction: Option<String>,
}

// Keep the permit inside the blocking worker: a timed-out syscall must not
// cause later UI/model/resource reads to accumulate unbounded probe threads.
static OBSERVATION_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// Only the paths and policy needed for a read, not a retained session/config.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservationRequest {
    targets: Vec<StorageTarget>,
    policy: MachineWarningsConfig,
    include_indexing: bool,
}

impl ObservationRequest {
    /// `cwd` is the active turn/UI workspace, not the process-global directory.
    pub fn new(config: &Config, cwd: &Path) -> Self {
        Self {
            targets: storage_targets(config, cwd, std::env::temp_dir()),
            policy: config.machine_warnings.clone(),
            include_indexing: false,
        }
    }

    /// Add read-only indexing probes for the active workspace to this read.
    pub fn with_indexing(mut self) -> Self {
        self.include_indexing = true;
        self
    }

    /// Fresh observations of this harness host, not an external MCP/tool host.
    /// The deadline includes queueing; timed-out workers retain the shared permit.
    pub async fn observe(&self) -> Result<MachineStatus, String> {
        let targets = self.targets.clone();
        let policy = self.policy.clone();
        let include_indexing = self.include_indexing;
        tokio::time::timeout(Duration::from_millis(policy.probe_timeout_ms), async move {
            let permit = OBSERVATION_GATE
                .acquire()
                .await
                .map_err(|_| "machine observation gate closed".to_string())?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let machine = chaos_machine::inspect_machine();
                let storage = chaos_machine::inspect_storage(&targets);
                let indexing = targets
                    .iter()
                    .find(|target| include_indexing && target.role == StorageRole::Workspace)
                    .map(|target| chaos_machine::inspect_indexing(&target.path));
                let warnings = machine_warnings::evaluate(&policy, &machine, &storage);
                let warning_instruction = machine_warnings::instructions(&warnings);
                MachineStatus {
                    machine,
                    storage,
                    indexing,
                    warnings,
                    warning_instruction,
                }
            })
            .await
            .map_err(|_| "machine observation worker failed".to_string())
        })
        .await
        .map_err(|_| "machine observations unavailable: probe timed out".to_string())?
    }
}

fn storage_targets(config: &Config, cwd: &Path, temporary: PathBuf) -> Vec<StorageTarget> {
    // Known working paths, not a drive inventory. Roles survive FS deduplication.
    let mut targets = Vec::new();
    // SQLite keeps state next to the database file named by the storage URL;
    // PostgreSQL holds it on the server, so no local directory is probed.
    let sqlite_state_dir = crate::runtime_db::sqlite_state_dir(&config.storage_url);
    for (role, path) in [
        Some((StorageRole::Workspace, cwd.to_path_buf())),
        Some((StorageRole::State, config.chaos_home.clone())),
        sqlite_state_dir.map(|dir| (StorageRole::State, dir)),
        Some((StorageRole::State, config.log_dir.clone())),
        Some((StorageRole::Temporary, temporary)),
    ]
    .into_iter()
    .flatten()
    {
        let target = StorageTarget { role, path };
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

#[cfg(test)]
mod tests;
