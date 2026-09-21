use std::sync::Arc;
use std::time::SystemTime;

use chaos_kern::machine_status::{MachineStatus, MachineWarning};
use chaos_machine::*;
use tokio::sync::watch;

use super::{clock, refresh_all, render, text, widgets};
use crate::top_bar::{BarWidget, Content, Side};

pub(in crate::top_bar) fn discharging(charge_percent: Option<u8>) -> PowerInfo {
    PowerInfo {
        source: PowerSource::Battery,
        external_power: Some(false),
        batteries: Some(vec![BatteryInfo {
            name: "BAT0".into(),
            kind: BatteryKind::System,
            present: Some(true),
            charge_percent,
            state: BatteryState::Discharging,
        }]),
    }
}

pub(in crate::top_bar) fn machine_status(power: PowerInfo) -> MachineStatus {
    MachineStatus {
        machine: MachineSnapshot {
            observed_at: SystemTime::UNIX_EPOCH,
            os: "linux",
            arch: "x86_64",
            profile: MachineProfile::default(),
            power,
            thermal: ThermalInfo::default(),
        },
        storage: StorageSnapshot {
            observed_at: SystemTime::UNIX_EPOCH,
            filesystems: vec![],
            unavailable: vec![],
        },
        warnings: vec![],
        warning_instruction: None,
    }
}

pub(in crate::top_bar) fn snapshot(power: PowerInfo) -> Option<Arc<MachineStatus>> {
    Some(Arc::new(machine_status(power)))
}

fn filesystem(available: u64) -> Filesystem {
    Filesystem {
        id: FilesystemId {
            device: 1,
            filesystem: 2,
        },
        filesystem_type: None,
        backing: FilesystemBacking::Other,
        total_bytes: 1000,
        available_bytes: available,
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
    }
}

fn sensor(kind: TemperatureKind, value: f64) -> CpuTemperature {
    CpuTemperature {
        id: "untrusted sensor name".into(),
        label: "untrusted label".into(),
        source: TemperatureSource::Hwmon,
        kind,
        celsius: Some(value),
        critical_celsius: None,
    }
}

#[test]
fn profile_keeps_chassis_display_and_session_independent() {
    let mut status = machine_status(PowerInfo::default());
    status.machine.profile.form_factor = FormFactor::Laptop;
    status.machine.profile.session.ssh = true;
    let (tx, rx) = watch::channel(Some(Arc::new(status.clone())));
    let mut bar = [widgets::profile::new(rx)];
    assert_eq!(text(&render(&bar, 12)), " laptop/SSH ");
    status.machine.profile.displays = DisplayState::NoneDetected;
    tx.send_replace(Some(Arc::new(status)));
    refresh_all(&mut bar);
    assert_eq!(text(&render(&bar, 21)), " laptop/headless/SSH ");
    tx.send_replace(None);
    refresh_all(&mut bar);
    assert_eq!(text(&render(&bar, 11)), " machine ? ");
}

#[test]
fn power_visibility_follows_desktop_classification() {
    let mut ups = discharging(Some(80));
    ups.source = PowerSource::Ups;
    ups.batteries.as_mut().unwrap()[0].kind = BatteryKind::Ups;

    for power in [
        PowerInfo::default(),
        PowerInfo {
            batteries: Some(vec![]),
            ..PowerInfo::default()
        },
        PowerInfo {
            source: PowerSource::External,
            external_power: Some(true),
            batteries: Some(vec![]),
        },
        discharging(Some(80)),
        ups,
    ] {
        let mut status = machine_status(power);
        status.machine.profile.form_factor = FormFactor::Desktop;
        let (tx, rx) = watch::channel(Some(Arc::new(status.clone())));
        let mut bar = [
            widgets::battery::new(rx),
            clock("2026-09-05T12:34:00Z[UTC]"),
        ];
        assert_eq!(bar[0].spec().min_width, 0);

        for form_factor in [
            FormFactor::Laptop,
            FormFactor::Tablet,
            FormFactor::Server,
            FormFactor::Other,
            FormFactor::Unknown,
            FormFactor::Desktop,
        ] {
            status.machine.profile.form_factor = form_factor;
            tx.send_replace(Some(Arc::new(status.clone())));
            refresh_all(&mut bar);
            if form_factor == FormFactor::Desktop {
                assert_eq!(bar[0].spec().min_width, 0);
                for width in [7, 30] {
                    assert_eq!(
                        render(&bar, width),
                        render(&bar[1..], width),
                        "hidden power must not take space or leave a separator"
                    );
                }
            } else {
                assert!(bar[0].spec().min_width > 0, "{form_factor:?}");
            }
        }
    }
}

#[test]
fn power_uses_primary_supply_and_keeps_battery_percentages_separate() {
    let (tx, rx) = watch::channel(None);
    let mut bar = [widgets::battery::new(rx)];
    for present in [Some(true), Some(false), None] {
        let mut power = discharging(Some(0));
        power.source = PowerSource::External;
        power.external_power = Some(true);
        power.batteries.as_mut().unwrap()[0].present = present;
        tx.send_replace(snapshot(power));
        refresh_all(&mut bar);
        let expected = [BarWidget::text(
            "expected",
            Side::Right,
            240,
            Content::new("⚡ AC"),
        )];
        assert_eq!(render(&bar, 20), render(&expected, 20));
        assert_eq!(
            bar[0].spec().priority,
            240,
            "AC must not raise a battery alert"
        );
    }
    let mut power = discharging(Some(5));
    let mut second = power.batteries.as_ref().unwrap()[0].clone();
    second.charge_percent = Some(80);
    power.batteries.as_mut().unwrap().push(second);
    tx.send_replace(snapshot(power.clone()));
    refresh_all(&mut bar);
    assert_eq!(text(&render(&bar, 10)), " ● 5%/80% ");
    assert_eq!(
        bar[0].spec().priority,
        240,
        "only kernel policy supplies warning tone"
    );

    power.source = PowerSource::Ups;
    power.batteries.as_mut().unwrap()[1].kind = BatteryKind::Ups;
    tx.send_replace(snapshot(power));
    refresh_all(&mut bar);
    assert_eq!(text(&render(&bar, 9)), " UPS 80% ");
    tx.send_replace(snapshot(PowerInfo::default()));
    refresh_all(&mut bar);
    assert_eq!(text(&render(&bar, 9)), " power ? ");
}

#[test]
fn disk_shows_tightest_relevant_filesystem_without_summing_or_hiding_failures() {
    let mut status = machine_status(discharging(Some(80)));
    let mut unused_full_drive = filesystem(0);
    unused_full_drive.targets.clear();
    status.storage.filesystems = vec![filesystem(500), filesystem(50), unused_full_drive];
    let (tx, rx) = watch::channel(Some(Arc::new(status.clone())));
    let mut bar = [widgets::disk::new(rx), clock("2026-09-05T12:34:00Z[UTC]")];
    assert_eq!(text(&render(&bar[..1], 16)), " disk 5.0% free ");
    let disk = &status.storage.filesystems[1];
    status.warnings.push(MachineWarning::LowDiskSpace {
        filesystem: disk.id,
        targets: disk
            .targets
            .iter()
            .map(|target| target.target.clone())
            .collect(),
        available_bytes: disk.available_bytes,
        available_percent: 5.0,
    });
    tx.send_replace(Some(Arc::new(status.clone())));
    refresh_all(&mut bar);
    assert_eq!(bar[0].spec().priority, 250);
    assert_eq!(
        text(&render(&bar, 16)),
        " disk 5.0% free ",
        "warning takes space before clock"
    );

    status.warnings.clear();
    status.storage.filesystems[1].available_bytes = 0;
    status.storage.unavailable.push(StorageFailure {
        target: StorageTarget {
            role: StorageRole::State,
            path: "/inaccessible".into(),
        },
        reason: "not read".into(),
    });
    tx.send_replace(Some(Arc::new(status.clone())));
    refresh_all(&mut bar);
    assert_eq!(text(&render(&bar[..1], 18)), " disk 0.0% free ? ");
    status.storage.filesystems.clear();
    tx.send_replace(Some(Arc::new(status)));
    refresh_all(&mut bar);
    assert_eq!(text(&render(&bar[..1], 8)), " disk ? ");
}

#[test]
fn thermals_never_relabel_control_scales_or_missing_data_as_physical_celsius() {
    let mut status = machine_status(PowerInfo::default());
    status.machine.thermal.cpu_temperatures = vec![
        sensor(TemperatureKind::Physical, 60.0),
        sensor(TemperatureKind::Physical, 70.0),
        sensor(TemperatureKind::Physical, f64::NAN),
        sensor(TemperatureKind::Control, 120.0),
        sensor(TemperatureKind::Unknown, 150.0),
    ];
    let (tx, rx) = watch::channel(Some(Arc::new(status.clone())));
    let mut bar = [widgets::thermal::new(rx)];
    assert_eq!(text(&render(&bar, 14)), " CPU max 70°C ");
    status.machine.thermal.cpu_temperatures.clear();
    tx.send_replace(Some(Arc::new(status.clone())));
    refresh_all(&mut bar);
    assert!(text(&render(&bar, 20)).trim().is_empty());

    for (state, label) in [
        (ThermalState::Warning, "heat warning"),
        (ThermalState::Critical, "heat critical"),
    ] {
        status.machine.thermal.state = state;
        status.warnings = vec![MachineWarning::Thermal { state }];
        tx.send_replace(Some(Arc::new(status.clone())));
        refresh_all(&mut bar);
        assert_eq!(text(&render(&bar, 20)).trim(), label);
        assert_eq!(bar[0].spec().priority, 250);
    }
    status.machine.thermal.state = ThermalState::Unknown;
    status.warnings = vec![MachineWarning::CpuTemperature {
        sensor: "untrusted".into(),
        temperature_kind: TemperatureKind::Control,
        value: 120.0,
        threshold: 115.0,
    }];
    tx.send_replace(Some(Arc::new(status)));
    refresh_all(&mut bar);
    assert_eq!(text(&render(&bar, 11)), " CPU limit ");
}

#[test]
fn registry_includes_live_machine_readings_and_clock() {
    let mut status = machine_status(discharging(Some(80)));
    status.machine.profile.form_factor = FormFactor::Laptop;
    status.storage.filesystems = vec![filesystem(500)];
    status.machine.thermal.cpu_temperatures = vec![sensor(TemperatureKind::Physical, 65.0)];
    let (_, source) = watch::channel(Some(Arc::new(status)));
    let (_, persistence) = watch::channel(chaos_kern::PersistenceStatus::default());
    let mut bar = widgets::initial_widgets("host".into(), source, persistence);
    refresh_all(&mut bar);
    let line = text(&render(&bar, 180));
    for label in [
        "host",
        "laptop",
        "disk 50.0% free",
        "CPU max 65°C",
        "● 80%",
        "12:34",
    ] {
        assert!(line.contains(label), "{line:?} missing {label}");
    }
    for width in 0..=180 {
        assert_eq!(
            crate::width::display_width(&text(&render(&bar, width))),
            usize::from(width)
        );
    }
}
