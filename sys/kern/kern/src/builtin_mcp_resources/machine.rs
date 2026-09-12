use std::path::Path;
use std::path::PathBuf;

use chaos_machine::MachineSnapshot;
use chaos_machine::StorageRole;
use chaos_machine::StorageSnapshot;
use chaos_machine::StorageTarget;
use serde::Serialize;

use crate::config::Config;

#[derive(Serialize)]
struct MachineResource {
    scope: &'static str,
    machine: MachineSnapshot,
    storage: StorageSnapshot,
}

/// Fresh observations of this harness host, not an external MCP/tool host.
///
/// `cwd` must come from the active turn, not the process-global working directory.
/// The standalone MCP server supplies its own configured cwd instead.
pub async fn machine_json(config: &Config, cwd: &Path) -> Result<String, String> {
    let targets = storage_targets(config, cwd, std::env::temp_dir());
    tokio::task::spawn_blocking(move || {
        let resource = MachineResource {
            scope: "harness_host",
            machine: chaos_machine::inspect_machine(),
            storage: chaos_machine::inspect_storage(&targets),
        };
        super::to_json(&resource, "machine")
    })
    .await
    .map_err(|_| "machine resource observation worker failed".to_string())?
}

fn storage_targets(config: &Config, cwd: &Path, temporary: PathBuf) -> Vec<StorageTarget> {
    // These are known working paths, not a mount/drive inventory. Distinct roles
    // survive filesystem deduplication so temporary data is never called durable.
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
}
