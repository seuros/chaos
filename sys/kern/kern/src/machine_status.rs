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
}

impl ObservationRequest {
    /// `cwd` is the active turn/UI workspace, not the process-global directory.
    pub fn new(config: &Config, cwd: &Path) -> Self {
        Self {
            targets: storage_targets(config, cwd, std::env::temp_dir()),
            policy: config.machine_warnings.clone(),
        }
    }

    /// Fresh observations of this harness host, not an external MCP/tool host.
    /// The deadline includes queueing; timed-out workers retain the shared permit.
    pub async fn observe(&self) -> Result<MachineStatus, String> {
        let targets = self.targets.clone();
        let policy = self.policy.clone();
        tokio::time::timeout(Duration::from_millis(policy.probe_timeout_ms), async move {
            let permit = OBSERVATION_GATE
                .acquire()
                .await
                .map_err(|_| "machine observation gate closed".to_string())?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let machine = chaos_machine::inspect_machine();
                let storage = chaos_machine::inspect_storage(&targets);
                let warnings = machine_warnings::evaluate(&policy, &machine, &storage);
                let warning_instruction = machine_warnings::instructions(&warnings);
                MachineStatus {
                    machine,
                    storage,
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
    for (role, path) in [
        (StorageRole::Workspace, cwd.to_path_buf()),
        (StorageRole::State, config.chaos_home.clone()),
        (StorageRole::State, config.sqlite_home.clone()),
        (StorageRole::State, config.log_dir.clone()),
        (StorageRole::Temporary, temporary),
    ] {
        let target = StorageTarget { role, path };
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn observation_deadline_includes_queueing_without_starting_a_host_probe() {
        let _permit = OBSERVATION_GATE.acquire().await.expect("observation gate");
        let request = ObservationRequest {
            targets: vec![],
            policy: MachineWarningsConfig {
                probe_timeout_ms: 25,
                ..Default::default()
            },
        };
        let started = tokio::time::Instant::now();
        let error = request
            .observe()
            .await
            .expect_err("queued probe must time out");
        assert_eq!(error, "machine observations unavailable: probe timed out");
        assert_eq!(started.elapsed(), Duration::from_millis(25));
    }

    #[test]
    fn storage_targets_use_turn_cwd_and_configured_paths_not_global_disks() {
        let mut config = crate::config::test_config();
        config.cwd = PathBuf::from("/unused-startup-cwd");
        config.chaos_home = PathBuf::from("/state");
        config.sqlite_home = PathBuf::from("/cache");
        config.log_dir = PathBuf::from("/logs");
        let targets = storage_targets(&config, Path::new("/active-workspace"), "/scratch".into());
        assert_eq!(
            targets,
            vec![
                StorageTarget {
                    role: StorageRole::Workspace,
                    path: "/active-workspace".into()
                },
                StorageTarget {
                    role: StorageRole::State,
                    path: "/state".into()
                },
                StorageTarget {
                    role: StorageRole::State,
                    path: "/cache".into()
                },
                StorageTarget {
                    role: StorageRole::State,
                    path: "/logs".into()
                },
                StorageTarget {
                    role: StorageRole::Temporary,
                    path: "/scratch".into()
                },
            ]
        );
    }

    #[test]
    fn repeated_paths_are_deduplicated_but_distinct_roles_are_preserved() {
        let mut config = crate::config::test_config();
        config.chaos_home = PathBuf::from("/shared");
        config.sqlite_home = config.chaos_home.clone();
        config.log_dir = config.chaos_home.clone();
        let targets = storage_targets(&config, Path::new("/shared"), "/shared".into());
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].role, StorageRole::Workspace);
        assert_eq!(targets[1].role, StorageRole::State);
        assert_eq!(targets[2].role, StorageRole::Temporary);
    }

    #[test]
    fn requests_track_policy_and_active_paths_not_unrelated_config() {
        let mut config = crate::config::test_config();
        let request = ObservationRequest::new(&config, Path::new("/active"));
        config.cwd = "/ignored-startup-cwd".into();
        assert_eq!(
            request,
            ObservationRequest::new(&config, Path::new("/active"))
        );
        assert_ne!(request, ObservationRequest::new(&config, Path::new("/new")));
        config.machine_warnings.battery_percent += 1;
        assert_ne!(
            request,
            ObservationRequest::new(&config, Path::new("/active"))
        );
    }
}
