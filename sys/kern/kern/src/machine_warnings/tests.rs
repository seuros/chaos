use super::*;
use chaos_machine::{
    BatteryInfo, CpuTemperature, Filesystem, FilesystemBacking, MachineProfile, PowerInfo,
    ResolvedStorageTarget, TemperatureSource, ThermalInfo,
};
use std::time::SystemTime;

fn machine() -> MachineSnapshot {
    MachineSnapshot {
        observed_at: SystemTime::now(),
        os: "test",
        arch: "test",
        profile: MachineProfile::default(),
        power: PowerInfo {
            source: PowerSource::Battery,
            external_power: Some(false),
            batteries: Some(vec![BatteryInfo {
                name: "BAT0".into(),
                kind: BatteryKind::System,
                present: Some(true),
                charge_percent: Some(5),
                state: BatteryState::Discharging,
            }]),
        },
        thermal: ThermalInfo::default(),
    }
}

fn storage(available_bytes: u64) -> StorageSnapshot {
    StorageSnapshot {
        observed_at: SystemTime::now(),
        filesystems: vec![Filesystem {
            id: FilesystemId {
                device: 1,
                filesystem: 1,
            },
            filesystem_type: Some("test".into()),
            backing: FilesystemBacking::Other,
            total_bytes: 10_000,
            available_bytes,
            available_inodes: None,
            read_only: false,
            targets: [
                StorageRole::Workspace,
                StorageRole::State,
                StorageRole::Temporary,
            ]
            .into_iter()
            .map(|role| ResolvedStorageTarget {
                target: StorageTarget {
                    role,
                    path: "/shared".into(),
                },
                probed_path: "/shared".into(),
            })
            .collect(),
        }],
        unavailable: vec![],
    }
}

fn evaluate_power(machine: &MachineSnapshot) -> Vec<MachineWarning> {
    evaluate(&MachineWarningsConfig::default(), machine, &storage(9_000))
}

#[test]
fn battery_threshold_is_inclusive_and_recovery_clears_it() {
    let mut machine = machine();
    for (percent, expected) in [(0, 1), (5, 1), (6, 0)] {
        machine.power.batteries.as_mut().unwrap()[0].charge_percent = Some(percent);
        assert_eq!(evaluate_power(&machine).len(), expected);
    }
    machine.power.batteries.as_mut().unwrap()[0].charge_percent = Some(5);
    machine.power.source = PowerSource::External;
    machine.power.external_power = Some(true);
    assert!(evaluate_power(&machine).is_empty());
}

#[test]
fn dead_removed_or_discharging_batteries_on_external_power_do_not_warn() {
    let mut machine = machine();
    machine.power.source = PowerSource::External;
    machine.power.external_power = Some(true);
    for present in [None, Some(false), Some(true)] {
        let battery = &mut machine.power.batteries.as_mut().unwrap()[0];
        battery.present = present;
        battery.charge_percent = Some(0);
        assert!(evaluate_power(&machine).is_empty());
    }
}

#[test]
fn unknown_inactive_absent_and_wrong_supply_batteries_do_not_warn() {
    let mut machine = machine();
    for state in [
        BatteryState::Charging,
        BatteryState::Full,
        BatteryState::Idle,
        BatteryState::Unknown,
    ] {
        machine.power.batteries.as_mut().unwrap()[0].state = state;
        assert!(evaluate_power(&machine).is_empty());
    }
    machine.power.batteries.as_mut().unwrap()[0].state = BatteryState::Discharging;
    machine.power.batteries.as_mut().unwrap()[0].present = Some(false);
    assert!(evaluate_power(&machine).is_empty());
    machine.power.batteries.as_mut().unwrap()[0].present = Some(true);
    machine.power.batteries.as_mut().unwrap()[0].charge_percent = None;
    assert!(evaluate_power(&machine).is_empty());
    machine.power.batteries.as_mut().unwrap()[0].charge_percent = Some(0);
    machine.power.source = PowerSource::Unknown;
    assert!(evaluate_power(&machine).is_empty());
    machine.power.source = PowerSource::Ups;
    assert!(evaluate_power(&machine).is_empty());
    machine.power.batteries = None;
    assert!(evaluate_power(&machine).is_empty());
}

#[test]
fn ups_and_multiple_batteries_remain_individual_not_combined_capacity() {
    let mut machine = machine();
    machine.power.source = PowerSource::Ups;
    machine.power.batteries.as_mut().unwrap()[0].kind = BatteryKind::Ups;
    assert!(matches!(
        evaluate_power(&machine)[0],
        MachineWarning::LowBattery {
            battery_kind: BatteryKind::Ups,
            ..
        }
    ));
    let mut healthy = machine.power.batteries.as_ref().unwrap()[0].clone();
    healthy.name = "UPS1".into();
    healthy.charge_percent = Some(95);
    machine.power.batteries.as_mut().unwrap().push(healthy);
    let warnings = evaluate_power(&machine);
    assert_eq!(warnings.len(), 1);
    assert!(
        instructions(&warnings)
            .unwrap()
            .contains("not combined capacity")
    );
}

#[test]
fn storage_is_per_relevant_filesystem_with_exact_threshold_comparison() {
    let mut machine = machine();
    machine.power = PowerInfo::default();
    for (bytes, expected) in [(0, 1), (500, 1), (501, 0)] {
        let warnings = evaluate(&MachineWarningsConfig::default(), &machine, &storage(bytes));
        assert_eq!(warnings.len(), expected);
        if let Some(MachineWarning::LowDiskSpace { targets, .. }) = warnings.first() {
            assert_eq!(targets.len(), 3); // shared filesystem warns once, preserves all roles
        }
    }
    let mut snapshot = storage(0);
    snapshot.filesystems[0].targets.clear();
    assert!(evaluate(&MachineWarningsConfig::default(), &machine, &snapshot).is_empty());
}

#[test]
fn missing_invalid_capacity_and_failed_probes_are_not_zero_free_space() {
    let mut machine = machine();
    machine.power = PowerInfo::default();
    let mut snapshot = storage(0);
    snapshot.filesystems[0].total_bytes = 0;
    snapshot.unavailable.push(chaos_machine::StorageFailure {
        target: StorageTarget {
            role: StorageRole::Output,
            path: "/unavailable".into(),
        },
        reason: "unavailable".into(),
    });
    assert!(evaluate(&MachineWarningsConfig::default(), &machine, &snapshot).is_empty());
    snapshot.filesystems[0].total_bytes = 10;
    snapshot.filesystems[0].available_bytes = 11;
    assert!(evaluate(&MachineWarningsConfig::default(), &machine, &snapshot).is_empty());
}

#[test]
fn large_filesystems_do_not_round_just_above_the_threshold_down_to_low_space() {
    let mut machine = machine();
    machine.power = PowerInfo::default();
    let mut snapshot = storage(0);
    snapshot.filesystems[0].total_bytes = 20_u64 << 56;
    snapshot.filesystems[0].available_bytes = snapshot.filesystems[0].total_bytes / 20;
    assert_eq!(
        evaluate(&MachineWarningsConfig::default(), &machine, &snapshot).len(),
        1
    );
    snapshot.filesystems[0].available_bytes += 1;
    assert!(evaluate(&MachineWarningsConfig::default(), &machine, &snapshot).is_empty());
}

#[test]
fn unknown_thermals_are_not_overheating_but_known_pressure_warns() {
    let mut machine = machine();
    machine.power = PowerInfo::default();
    for (state, count) in [
        (ThermalState::Unknown, 0),
        (ThermalState::Normal, 0),
        (ThermalState::Warning, 1),
        (ThermalState::Critical, 1),
    ] {
        machine.thermal.state = state;
        assert_eq!(evaluate_power(&machine).len(), count);
    }
}

#[test]
fn custom_temperature_threshold_only_applies_to_physical_channels() {
    let mut machine = machine();
    machine.power = PowerInfo::default();
    machine.thermal.cpu_temperatures.push(CpuTemperature {
        id: "cpu".into(),
        label: "CPU".into(),
        source: TemperatureSource::Hwmon,
        kind: TemperatureKind::Physical,
        celsius: Some(90.0),
        critical_celsius: None,
    });
    let config = MachineWarningsConfig {
        cpu_temperature_celsius: Some(90.0),
        ..Default::default()
    };
    assert_eq!(evaluate(&config, &machine, &storage(9_000)).len(), 1);
    machine.thermal.cpu_temperatures[0].celsius = Some(89.9);
    assert!(evaluate(&config, &machine, &storage(9_000)).is_empty());
    machine.thermal.cpu_temperatures[0].celsius = Some(90.0);
    for kind in [TemperatureKind::Control, TemperatureKind::Unknown] {
        machine.thermal.cpu_temperatures[0].kind = kind;
        assert!(evaluate(&config, &machine, &storage(9_000)).is_empty());
        machine.thermal.cpu_temperatures[0].critical_celsius = Some(90.0);
        assert_eq!(evaluate(&config, &machine, &storage(9_000)).len(), 1);
        machine.thermal.cpu_temperatures[0].critical_celsius = None;
    }
    machine.thermal.cpu_temperatures[0].kind = TemperatureKind::Physical;
    for value in [None, Some(f64::NAN), Some(f64::INFINITY)] {
        machine.thermal.cpu_temperatures[0].celsius = value;
        assert!(evaluate(&config, &machine, &storage(9_000)).is_empty());
    }
}

#[test]
fn thresholds_are_configurable_and_warnings_can_be_disabled() {
    let mut machine = machine();
    machine.thermal.state = ThermalState::Warning;
    let config = MachineWarningsConfig {
        battery_percent: 4,
        disk_free_percent: 4,
        thermal: false,
        ..Default::default()
    };
    assert!(evaluate(&config, &machine, &storage(500)).is_empty());
    let config = MachineWarningsConfig {
        enabled: false,
        ..Default::default()
    };
    assert!(evaluate(&config, &machine, &storage(0)).is_empty());
}

#[test]
fn warning_instructions_checkpoint_and_delegate_hardware_actions_without_raw_names() {
    let mut machine = machine();
    machine.power.batteries.as_mut().unwrap()[0].name = "UNTRUSTED_NAME".into();
    machine.thermal.state = ThermalState::Critical;
    let mut snapshot = storage(0);
    snapshot.filesystems[0].targets[0].target.path = "/UNTRUSTED_PATH".into();
    let warnings = evaluate(&MachineWarningsConfig::default(), &machine, &snapshot);
    let message = instructions(&warnings).unwrap();
    for expected in [
        "harness host",
        "minimal checkpoint",
        "persistent storage",
        "outside /tmp",
        "verify the write succeeded",
        "Do not rely on suspend",
        "Tell the operator to restore external power, make storage space available, check cooling",
        "Do not delete files",
        "flashing",
        "workspace/state/temporary",
    ] {
        assert!(message.contains(expected), "{expected}");
    }
    assert!(!message.contains("UNTRUSTED"));
    assert!(instructions(&[]).is_none());
}
