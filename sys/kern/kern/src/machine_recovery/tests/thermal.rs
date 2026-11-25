use super::super::*;
use super::fixtures::*;

#[test]
fn recovery_needs_all_headroom_and_five_minutes_not_a_single_good_reading() {
    let config = MachineWarningsConfig::default();
    let mut recovery = Recovery::default();
    sample(&mut recovery, &config, snapshot(&config, 90.0, 5 << 30), 0);
    assert_eq!(recovery.phase(), Phase::Warning);
    let instruction = recovery.instruction(&config, Duration::ZERO).unwrap();
    assert!(!instruction.is_empty());
    assert!(!instruction.contains("UNTRUSTED"));
    let id = recovery.request_wait("turn").unwrap();
    assert_eq!(recovery.request_wait("turn").unwrap(), id);
    // A few MB and a tiny temperature drop clear raw warnings, not the episode.
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 89.9, (5 << 30) + (4 << 20)),
        30,
    );
    assert_eq!(recovery.phase(), Phase::Warning);
    for seconds in (60..360).step_by(30) {
        sample(
            &mut recovery,
            &config,
            snapshot(&config, 85.0, 7 << 30),
            seconds,
        );
        assert_eq!(recovery.phase(), Phase::Stabilizing);
    }
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 85.0, 7 << 30),
        360,
    );
    assert_eq!(recovery.phase(), Phase::Recovered);
    assert_eq!(
        recovery
            .status(&config, Duration::from_secs(360))
            .stable_seconds,
        300
    );
    assert_eq!(
        recovery.instruction(&config, Duration::from_secs(360)),
        None
    );

    sample(
        &mut recovery,
        &config,
        snapshot(&config, 89.9, 7 << 30),
        390,
    );
    assert_eq!(
        recovery.phase(),
        Phase::Recovered,
        "a dip under the threshold"
    );
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 90.0, 7 << 30),
        420,
    );
    assert_eq!(recovery.phase(), Phase::Warning);
    assert_eq!(recovery.episode().map(Episode::raw), Some(2));
}

#[test]
fn cpu_recovery_sits_one_margin_below_the_triggering_threshold() {
    let config = MachineWarningsConfig {
        recovery_temperature_margin_celsius: 2.5,
        ..Default::default()
    };
    let warning = snapshot(&config, 90.0, 20 << 30).warnings.remove(0);
    for (celsius, recovered) in [(89.0, false), (87.6, false), (87.5, true)] {
        let status = snapshot(&config, celsius, 20 << 30);
        assert_eq!(verdict(&warning, &status, &config), Ok(recovered));
    }
}

#[test]
fn thermal_unknown_and_filesystem_remount_are_not_recovery() {
    let config = MachineWarningsConfig::default();
    let mut status = snapshot(&config, 80.0, 20 << 30);
    status.machine.thermal.state = ThermalState::Unknown;
    let thermal = MachineWarning::Thermal {
        state: ThermalState::Critical,
    };
    assert!(verdict(&thermal, &status, &config).is_err());
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
            verdict(&low.warnings[0], &missing, &config).is_err(),
            "case {failure}"
        );
    }
}

#[test]
fn thermal_keeps_the_long_window_while_power_and_disk_recover_quickly() {
    let config = MachineWarningsConfig::default();

    let mut power = Recovery::default();
    sample(&mut power, &config, snapshot(&config, 70.0, 1 << 30), 0);
    assert_eq!(power.phase(), Phase::Warning);
    sample(&mut power, &config, snapshot(&config, 70.0, 7 << 30), 10);
    assert_eq!(
        power.phase(),
        Phase::Stabilizing,
        "window starts at first headroom"
    );
    sample(
        &mut power,
        &config,
        snapshot(&config, 70.0, 7 << 30),
        10 + config.recovery_instant_seconds,
    );
    assert_eq!(
        power.required_seconds(&config),
        config.recovery_instant_seconds
    );
    assert_eq!(power.phase(), Phase::Recovered);

    let mut thermal = Recovery::default();
    sample(&mut thermal, &config, snapshot(&config, 95.0, 20 << 30), 0);
    sample(&mut thermal, &config, snapshot(&config, 70.0, 20 << 30), 10);
    assert_eq!(
        thermal.required_seconds(&config),
        config.recovery_stable_seconds
    );
    assert_eq!(thermal.phase(), Phase::Stabilizing);
}
