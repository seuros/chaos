use super::super::*;
use chaos_machine::*;

pub(super) fn snapshot(
    config: &MachineWarningsConfig,
    temperature: f64,
    available: u64,
) -> MachineStatus {
    snapshot_on_disk(config, temperature, 100 << 30, available)
}

pub(super) fn snapshot_on_disk(
    config: &MachineWarningsConfig,
    temperature: f64,
    total: u64,
    available: u64,
) -> MachineStatus {
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
            ..Default::default()
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
            total_bytes: total,
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
        indexing: None,
        machine,
        storage,
        warning_instruction: crate::machine_warnings::instructions(&warnings),
        warnings,
    }
}

pub(super) fn sample(
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
