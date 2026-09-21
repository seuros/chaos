use super::*;
use chaos_machine::*;

fn snapshot(config: &MachineWarningsConfig, temperature: f64, available: u64) -> MachineStatus {
    let machine = MachineSnapshot {
        observed_at: SystemTime::UNIX_EPOCH,
        os: "test",
        arch: "test",
        profile: MachineProfile::default(),
        power: PowerInfo {
            source: PowerSource::External,
            external_power: Some(true),
            batteries: None,
        },
        thermal: ThermalInfo {
            state: ThermalState::Normal,
            cpu_temperatures: vec![CpuTemperature {
                id: "cpu".into(),
                label: "UNTRUSTED".into(),
                source: TemperatureSource::Hwmon,
                kind: TemperatureKind::Physical,
                celsius: Some(temperature),
                critical_celsius: Some(90.0),
            }],
        },
    };
    let storage = StorageSnapshot {
        observed_at: SystemTime::UNIX_EPOCH,
        filesystems: vec![Filesystem {
            id: FilesystemId {
                device: 1,
                filesystem: 1,
            },
            filesystem_type: None,
            backing: FilesystemBacking::Other,
            total_bytes: 100 << 30,
            available_bytes: available,
            available_inodes: None,
            read_only: false,
            targets: vec![ResolvedStorageTarget {
                target: StorageTarget {
                    role: StorageRole::Workspace,
                    path: "/UNTRUSTED".into(),
                },
                probed_path: "/UNTRUSTED".into(),
            }],
        }],
        unavailable: vec![],
    };
    let warnings = crate::machine_warnings::evaluate(config, &machine, &storage);
    MachineStatus {
        machine,
        storage,
        warning_instruction: crate::machine_warnings::instructions(&warnings),
        warnings,
    }
}

fn sample(
    recovery: &mut Recovery,
    config: &MachineWarningsConfig,
    status: MachineStatus,
    start: Instant,
    seconds: u64,
) {
    recovery.observe(
        &Ok(status),
        config,
        start + Duration::from_secs(seconds),
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
    );
}

#[test]
fn recovery_needs_all_headroom_and_five_minutes_not_a_single_good_reading() {
    let config = MachineWarningsConfig::default();
    let start = Instant::now();
    let mut recovery = Recovery::default();
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 90.0, 5 << 30),
        start,
        0,
    );
    assert_eq!(recovery.phase, Phase::Warning);
    let id = recovery.request_wait("turn").unwrap();
    assert_eq!(recovery.request_wait("turn").unwrap(), id);
    // A few MB and a tiny temperature drop clear raw warnings, not the episode.
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 89.9, (5 << 30) + (4 << 20)),
        start,
        30,
    );
    assert_eq!(recovery.phase, Phase::Warning);
    for seconds in (60..360).step_by(30) {
        sample(
            &mut recovery,
            &config,
            snapshot(&config, 85.0, 7 << 30),
            start,
            seconds,
        );
        assert_eq!(recovery.phase, Phase::Stabilizing);
    }
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 85.0, 7 << 30),
        start,
        360,
    );
    assert_eq!(recovery.phase, Phase::Recovered);
    assert_eq!(
        recovery
            .status(&config, start + Duration::from_secs(360))
            .stable_seconds,
        300
    );
    assert!(
        !recovery
            .instruction(&config, start)
            .unwrap()
            .contains("UNTRUSTED")
    );
}

#[test]
fn renewed_warnings_probe_failures_missing_readings_and_suspend_reset_stability() {
    let config = MachineWarningsConfig::default();
    let start = Instant::now();
    for failure in 0..5 {
        let mut recovery = Recovery::default();
        sample(
            &mut recovery,
            &config,
            snapshot(&config, 90.0, 20 << 30),
            start,
            0,
        );
        for seconds in (30..300).step_by(30) {
            sample(
                &mut recovery,
                &config,
                snapshot(&config, 80.0, 20 << 30),
                start,
                seconds,
            );
        }
        let mut status = snapshot(&config, 80.0, 20 << 30);
        match failure {
            0 => sample(
                &mut recovery,
                &config,
                snapshot(&config, 90.0, 20 << 30),
                start,
                300,
            ),
            1 => recovery.observe(
                &Err("probe failed".into()),
                &config,
                start + Duration::from_secs(300),
                SystemTime::UNIX_EPOCH + Duration::from_secs(300),
            ),
            2 => {
                status.machine.thermal.cpu_temperatures.clear();
                sample(&mut recovery, &config, status, start, 300);
            }
            3 => sample(&mut recovery, &config, status, start, 400),
            _ => recovery.observe(
                &Ok(status),
                &config,
                start + Duration::from_secs(300),
                SystemTime::UNIX_EPOCH + Duration::from_secs(900),
            ),
        }
        assert_ne!(recovery.phase, Phase::Recovered, "case {failure}");
        assert_eq!(
            recovery
                .status(&config, start + Duration::from_secs(900))
                .stable_seconds,
            0
        );
    }
}

#[test]
fn power_requires_positive_external_power_not_battery_disappearance() {
    let config = MachineWarningsConfig::default();
    let mut status = snapshot(&config, 70.0, 20 << 30);
    let warning = MachineWarning::LowBattery {
        name: "UPS".into(),
        battery_kind: BatteryKind::Ups,
        charge_percent: 1,
    };
    status.machine.power = PowerInfo::default();
    assert!(recovered(&warning, &status, &config).is_err());
    status.machine.power.source = PowerSource::Ups;
    status.machine.power.external_power = Some(false);
    assert_eq!(recovered(&warning, &status, &config), Ok(false));
    status.machine.power.source = PowerSource::External;
    status.machine.power.external_power = Some(true);
    assert_eq!(recovered(&warning, &status, &config), Ok(true));
}

#[test]
fn thermal_unknown_and_filesystem_remount_are_not_recovery() {
    let config = MachineWarningsConfig::default();
    let mut status = snapshot(&config, 80.0, 20 << 30);
    status.machine.thermal.state = ThermalState::Unknown;
    assert!(
        recovered(
            &MachineWarning::Thermal {
                state: ThermalState::Critical
            },
            &status,
            &config
        )
        .is_err()
    );
    let low = snapshot(&config, 80.0, 1 << 30);
    status.storage.filesystems[0].id.device = 2;
    assert!(recovered(&low.warnings[0], &status, &config).is_err());
}

#[test]
fn disk_requires_both_margins_and_reports_impossible_targets() {
    let config = MachineWarningsConfig::default();
    let mut status = snapshot(&config, 70.0, 20 << 30);
    // On a small filesystem the absolute byte margin is the stronger requirement.
    status.storage.filesystems[0].total_bytes = 10 << 30;
    status.storage.filesystems[0].available_bytes = 1 << 30;
    let warning = snapshot(&config, 70.0, 1 << 30).warnings.remove(0);
    assert_eq!(recovered(&warning, &status, &config), Ok(false));
    status.storage.filesystems[0].available_bytes = 2 << 30;
    assert_eq!(recovered(&warning, &status, &config), Ok(true));
    status.storage.filesystems[0].total_bytes = 512 << 20;
    status.storage.filesystems[0].available_bytes = 400 << 20;
    assert_eq!(
        recovered(&warning, &status, &config),
        Err("recovery target exceeds filesystem capacity")
    );
}

#[test]
fn episodes_not_samples_are_counted_and_no_wait_is_implicit() {
    let config = MachineWarningsConfig::default();
    let start = Instant::now();
    let mut recovery = Recovery::default();
    assert!(recovery.request_wait("turn").is_err());
    for episode in 0..3 {
        let offset = episode * 360;
        sample(
            &mut recovery,
            &config,
            snapshot(&config, 90.0, 20 << 30),
            start,
            offset,
        );
        for seconds in (30..=330).step_by(30) {
            sample(
                &mut recovery,
                &config,
                snapshot(&config, 80.0, 20 << 30),
                start,
                offset + seconds,
            );
        }
        assert_eq!(recovery.phase, Phase::Recovered);
        assert!(recovery.wait_id.is_none());
        assert!(!recovery.parked);
        // Margin loss before a renewed alarm must not hide the next episode.
        sample(
            &mut recovery,
            &config,
            snapshot(&config, 86.0, 20 << 30),
            start,
            offset + 345,
        );
    }
    let status = recovery.status(&config, start + Duration::from_secs(1100));
    assert_eq!(status.interruptions_last_hour[1].count, 3);
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 90.0, 20 << 30),
        start,
        1100,
    );
    recovery.request_wait("new turn").unwrap();
    assert!(recovery.cancel_wait().is_some());
    assert!(recovery.wait_id.is_none());
}

#[test]
fn a_new_warning_joins_the_outstanding_set_and_restarts_the_timer() {
    let config = MachineWarningsConfig::default();
    let start = Instant::now();
    let mut recovery = Recovery::default();
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 90.0, 20 << 30),
        start,
        0,
    );
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 80.0, 20 << 30),
        start,
        30,
    );
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 80.0, 1 << 30),
        start,
        60,
    );
    assert_eq!(recovery.phase, Phase::Warning);
    assert_eq!(recovery.outstanding.len(), 2);
    assert_eq!(recovery.status(&config, start).stable_seconds, 0);
}

#[test]
fn operator_input_during_a_model_sample_prevents_a_late_wait_registration() {
    let config = MachineWarningsConfig::default();
    let mut recovery = Recovery::default();
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 90.0, 20 << 30),
        Instant::now(),
        0,
    );
    recovery.begin_sample();
    recovery.cancel_wait(); // New owner input, even before the tool was dispatched.
    assert!(recovery.request_wait("old response").is_err());
    recovery.begin_sample();
    assert!(recovery.request_wait("new response").is_ok());
}
