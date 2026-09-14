use super::*;
use chaos_machine::{
    Filesystem, FilesystemBacking, FilesystemId, MachineSnapshot, ResolvedStorageTarget,
    StorageRole, StorageSnapshot, StorageTarget,
};
use serde_json::{Value, json};
use std::cell::Cell;
use std::future::ready;
use std::time::{Duration, SystemTime};

fn observation(config: &Config, cwd: &Path, observed_at: SystemTime) -> MachineStatus {
    let machine = MachineSnapshot {
        observed_at,
        os: "test",
        arch: "test",
        profile: Default::default(),
        power: Default::default(),
        thermal: Default::default(),
    };
    let storage = StorageSnapshot {
        observed_at,
        filesystems: vec![Filesystem {
            id: FilesystemId {
                device: 1,
                filesystem: 1,
            },
            filesystem_type: Some("test".into()),
            backing: FilesystemBacking::Other,
            total_bytes: 100,
            available_bytes: 1,
            available_inodes: None,
            read_only: false,
            targets: vec![ResolvedStorageTarget {
                target: StorageTarget {
                    role: StorageRole::Workspace,
                    path: cwd.into(),
                },
                probed_path: cwd.into(),
            }],
        }],
        unavailable: vec![],
    };
    let warnings = crate::machine_warnings::evaluate(&config.machine_warnings, &machine, &storage);
    let warning_instruction = crate::machine_warnings::instructions(&warnings);
    MachineStatus {
        machine,
        storage,
        warnings,
        warning_instruction,
    }
}

#[tokio::test]
async fn machine_resource_reads_are_fresh_and_use_the_active_turn_directory() {
    let mut config = crate::config::test_config();
    config.cwd = "/unused-startup-cwd".into();
    config.chaos_home = "/state".into();
    config.sqlite_home = "/cache".into();
    config.log_dir = "/logs".into();
    let calls = Cell::new(0);

    for (index, (name, enabled, threshold, warns)) in [
        ("first-workspace", true, 100, true),
        ("first-workspace", true, 100, true),
        ("second-workspace", true, 0, false),
        ("second-workspace", false, 100, false),
    ]
    .into_iter()
    .enumerate()
    {
        let cwd = Path::new("/").join(name);
        // Explicit reads still return observations when automatic warnings are disabled.
        config.machine_warnings.enabled = enabled;
        config.machine_warnings.disk_free_percent = threshold;
        let expected_request = ObservationRequest::new(&config, &cwd);
        let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(index as u64 + 1);
        let text = machine_json_with_observer(&config, &cwd, |request| {
            calls.set(calls.get() + 1);
            assert_eq!(
                request, expected_request,
                "use this read's policy and paths"
            );
            ready(Ok(observation(&config, &cwd, observed_at)))
        })
        .await
        .expect("machine JSON");

        assert_eq!(calls.get(), index + 1, "every read must collect again");
        assert!(!text.contains('\n'), "resource JSON stays compact");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["scope"], "harness_host");
        assert_eq!(value["machine"]["observed_at"], json!(observed_at));
        assert!(value["machine"]["profile"].is_object());
        assert!(value["machine"]["power"].is_object());
        assert!(value["machine"]["thermal"]["state"].is_string());
        assert!(value["machine"]["thermal"]["cpu_temperatures"].is_array());
        assert_eq!(
            value["storage"]["filesystems"][0]["targets"][0]["target"],
            json!({"role": "workspace", "path": cwd})
        );
        assert_eq!(value["storage"]["unavailable"], json!([]));
        if warns {
            assert_eq!(value["warnings"][0]["kind"], "low_disk_space");
            assert!(
                value["warning_instruction"]
                    .as_str()
                    .unwrap()
                    .contains("minimal checkpoint")
            );
        } else {
            assert_eq!(value["warnings"], json!([]));
            assert!(value.get("warning_instruction").is_none());
        }
    }
}

#[tokio::test]
async fn machine_resource_propagates_observation_failure_without_a_snapshot() {
    let config = crate::config::test_config();
    let error = machine_json_with_observer(&config, Path::new("/active"), |_| {
        ready(Err("probe unavailable".into()))
    })
    .await
    .expect_err("failed observations must not become machine measurements");
    assert_eq!(error, "probe unavailable");
}
