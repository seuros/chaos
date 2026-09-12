use crate::power::{percent, source_from_supplies};
use crate::profile::classify;
use crate::{
    BatteryInfo, BatteryKind, BatteryState, DisplayState, ExecutionEnvironment, MachineProfile,
    PowerInfo, PowerSource,
};
use std::fs;
use std::path::Path;

mod thermal;
#[cfg(target_os = "linux")]
pub(crate) use thermal::read as read_thermal;

pub(crate) fn detect() -> (MachineProfile, PowerInfo) {
    detect_at(Path::new("/"))
}

fn text(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn boolean(path: &Path) -> Option<bool> {
    match text(path).as_deref() {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

fn detect_at(root: &Path) -> (MachineProfile, PowerInfo) {
    let cgroup = text(&root.join("proc/1/cgroup")).unwrap_or_default();
    let container = root.join(".dockerenv").exists()
        || root.join("run/.containerenv").exists()
        || text(&root.join("run/systemd/container")).is_some_and(|value| !value.is_empty())
        || ["/docker/", "/docker-", "/kubepods", "/lxc/"]
            .iter()
            .any(|marker| cgroup.contains(marker));
    if container {
        return (
            MachineProfile {
                execution_environment: ExecutionEnvironment::Container,
                ..MachineProfile::default()
            },
            PowerInfo::default(),
        );
    }

    let dmi = root.join("sys/class/dmi/id");
    let model = text(&dmi.join("product_name")).filter(|value| !value.is_empty());
    let virtual_machine = model.as_deref().is_some_and(|value| {
        [
            "KVM",
            "QEMU",
            "VirtualBox",
            "VMware Virtual Platform",
            "Virtual Machine",
            "HVM domU",
            "Bochs",
        ]
        .iter()
        .any(|prefix| value.starts_with(prefix))
    });
    let power = read_power(&root.join("sys/class/power_supply"));
    let mut profile = MachineProfile {
        model,
        displays: read_displays(&root.join("sys/class/drm")),
        execution_environment: if virtual_machine {
            ExecutionEnvironment::VirtualMachine
        } else {
            ExecutionEnvironment::NoneDetected
        },
        ..MachineProfile::default()
    };
    if !virtual_machine {
        let chassis = text(&dmi.join("chassis_type")).and_then(|value| value.parse().ok());
        (profile.form_factor, profile.form_factor_evidence) =
            classify(chassis, profile.model.as_deref(), &power);
    }
    (profile, power)
}

fn read_power(root: &Path) -> PowerInfo {
    let Ok(entries) = fs::read_dir(root) else {
        return PowerInfo::default();
    };
    let Ok(mut entries) = entries.collect::<Result<Vec<_>, _>>() else {
        return PowerInfo::default();
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut batteries = Vec::new();
    let mut complete = true;
    let mut adapters = Vec::new();
    for entry in entries {
        let path = entry.path();
        let scope = fs::read_to_string(path.join("scope"));
        match scope {
            Ok(scope) if scope.trim() == "Device" => continue,
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                complete = false;
                continue;
            }
            _ => {}
        }
        let Some(kind) = text(&path.join("type")) else {
            complete = false;
            continue;
        };
        match kind.as_str() {
            "Battery" | "UPS" => {
                let present = boolean(&path.join("present"));
                let charge_percent = text(&path.join("capacity")).and_then(|value| percent(&value));
                let state = match text(&path.join("status")).as_deref() {
                    Some("Charging") => BatteryState::Charging,
                    Some("Discharging") => BatteryState::Discharging,
                    Some("Full") => BatteryState::Full,
                    Some("Not charging") => BatteryState::Idle,
                    _ => BatteryState::Unknown,
                };
                // Some drivers omit `present` but provide live battery data.
                let present = present.or_else(|| {
                    (charge_percent.is_some() || state != BatteryState::Unknown).then_some(true)
                });
                batteries.push(BatteryInfo {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    kind: if kind == "UPS" {
                        BatteryKind::Ups
                    } else {
                        BatteryKind::System
                    },
                    present,
                    charge_percent: if present == Some(false) {
                        None
                    } else {
                        charge_percent
                    },
                    state: if present == Some(false) {
                        BatteryState::Unknown
                    } else {
                        state
                    },
                });
            }
            "Mains" | "USB" | "USB_C" | "USB_PD" | "USB_PD_DRP" | "USB_DCP" | "USB_CDP"
            | "Wireless" => adapters.push(boolean(&path.join("online"))),
            _ => {}
        }
    }
    let external_power = if adapters.contains(&Some(true)) {
        Some(true)
    } else if !adapters.is_empty() && adapters.iter().all(|value| *value == Some(false)) {
        Some(false)
    } else {
        None
    };
    let source = source_from_supplies(external_power, Some(&batteries));
    PowerInfo {
        // Partial enumeration can establish discharge but cannot exclude it.
        source: if !complete && source == PowerSource::External {
            PowerSource::Unknown
        } else {
            source
        },
        external_power,
        batteries: complete.then_some(batteries),
    }
}

fn read_displays(root: &Path) -> DisplayState {
    let Ok(entries) = fs::read_dir(root) else {
        return DisplayState::Unknown;
    };
    let mut observed = false;
    let mut incomplete = false;
    for entry in entries {
        let Ok(entry) = entry else {
            incomplete = true;
            continue;
        };
        // card0 is not a connector; card0-eDP-1/card0-HDMI-A-1 are.
        if !entry.file_name().to_string_lossy().contains('-') {
            continue;
        }
        match text(&entry.path().join("status")).as_deref() {
            Some("connected") => return DisplayState::Connected,
            Some("disconnected") => observed = true,
            _ => incomplete = true,
        }
    }
    if observed && !incomplete {
        DisplayState::NoneDetected
    } else {
        DisplayState::Unknown
    }
}

#[cfg(test)]
mod tests;
