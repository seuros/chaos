use super::*;

#[tokio::test(start_paused = true)]
async fn observation_deadline_includes_queueing_without_starting_a_host_probe() {
    let _permit = OBSERVATION_GATE.acquire().await.expect("observation gate");
    let request = ObservationRequest {
        targets: vec![],
        include_indexing: false,
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
    config.storage_home = PathBuf::from("/cache");
    config.storage_url = "sqlite:///cache/chaos.sqlite".to_string();
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
            // The SQLite file's directory, taken from the storage URL.
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
    config.storage_home = config.chaos_home.clone();
    config.storage_url = "sqlite:///shared/chaos.sqlite".to_string();
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
    assert!(
        !request.include_indexing,
        "automatic checks do not run indexing probes"
    );
    let explicit = request.clone().with_indexing();
    assert!(explicit.include_indexing);
    assert_ne!(request, explicit);
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

#[test]
fn postgres_storage_adds_no_local_state_directory() {
    let mut config = crate::config::test_config();
    config.chaos_home = PathBuf::from("/state");
    config.storage_url = "postgres://chaos@db.example/chaos".to_string();
    config.log_dir = PathBuf::from("/logs");
    let targets = storage_targets(&config, Path::new("/ws"), "/scratch".into());
    let state: Vec<_> = targets
        .iter()
        .filter(|target| target.role == StorageRole::State)
        .map(|target| target.path.clone())
        .collect();
    assert_eq!(state, vec![PathBuf::from("/state"), PathBuf::from("/logs")]);
}
