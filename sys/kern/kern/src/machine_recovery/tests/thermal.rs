use super::super::*;
use super::fixtures::*;

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
    assert_eq!(recovery.phase(), Phase::Warning);
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
    assert_eq!(recovery.phase(), Phase::Warning);
    for seconds in (60..360).step_by(30) {
        sample(
            &mut recovery,
            &config,
            snapshot(&config, 85.0, 7 << 30),
            start,
            seconds,
        );
        assert_eq!(recovery.phase(), Phase::Stabilizing);
    }
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 85.0, 7 << 30),
        start,
        360,
    );
    assert_eq!(recovery.phase(), Phase::Recovered);
    assert_eq!(
        recovery
            .status(&config, start + Duration::from_secs(360))
            .stable_seconds,
        300
    );
    let instruction = recovery.instruction(&config, start).unwrap();
    assert!(!instruction.is_empty());
    assert!(!instruction.contains("UNTRUSTED"));
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
    for failure in 0..7 {
        let mut missing = status.clone();
        let filesystem = &mut missing.storage.filesystems[0];
        match failure {
            0 => missing.storage.filesystems.clear(),
            1 => filesystem.id.device = 2,
            2 => filesystem.id.filesystem = 2,
            3 => filesystem.targets.clear(),
            4 => filesystem.total_bytes = 0,
            5 => filesystem.available_bytes = filesystem.total_bytes + 1,
            _ => filesystem.targets[0].target.path = "/remounted".into(),
        }
        assert!(
            recovered(&low.warnings[0], &missing, &config).is_err(),
            "case {failure}"
        );
    }
}

#[test]
fn thermal_keeps_the_long_window_while_power_and_disk_recover_quickly() {
    let config = MachineWarningsConfig::default();
    let start = Instant::now();

    let mut power = Recovery::default();
    sample(
        &mut power,
        &config,
        snapshot(&config, 70.0, 1 << 30),
        start,
        0,
    );
    assert_eq!(power.phase(), Phase::Warning);
    sample(
        &mut power,
        &config,
        snapshot(&config, 70.0, 7 << 30),
        start,
        10,
    );
    assert_eq!(
        power.phase(),
        Phase::Stabilizing,
        "window starts at first headroom"
    );
    sample(
        &mut power,
        &config,
        snapshot(&config, 70.0, 7 << 30),
        start,
        10 + config.recovery_instant_seconds,
    );
    assert_eq!(
        power.required_seconds(&config),
        config.recovery_instant_seconds
    );
    assert_eq!(power.phase(), Phase::Recovered);

    let mut thermal = Recovery::default();
    sample(
        &mut thermal,
        &config,
        snapshot(&config, 95.0, 20 << 30),
        start,
        0,
    );
    sample(
        &mut thermal,
        &config,
        snapshot(&config, 70.0, 20 << 30),
        start,
        10,
    );
    assert_eq!(
        thermal.required_seconds(&config),
        config.recovery_stable_seconds
    );
    assert_eq!(thermal.phase(), Phase::Stabilizing);
}
