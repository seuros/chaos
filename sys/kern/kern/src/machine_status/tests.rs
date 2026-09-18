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
