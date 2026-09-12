//! Warning policy belongs to the kernel; chaos-machine only reports observations.

use chaos_machine::{
    BatteryKind, BatteryState, FilesystemId, MachineSnapshot, PowerSource, StorageRole,
    StorageSnapshot, StorageTarget, TemperatureKind, ThermalState,
};
use serde::Serialize;

use crate::config::MachineWarningsConfig;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineWarning {
    LowBattery {
        name: String,
        battery_kind: BatteryKind,
        charge_percent: u8,
    },
    LowDiskSpace {
        filesystem: FilesystemId,
        targets: Vec<StorageTarget>,
        available_bytes: u64,
        available_percent: f64,
    },
    Thermal {
        state: ThermalState,
    },
    CpuTemperature {
        sensor: String,
        temperature_kind: TemperatureKind,
        value: f64,
        threshold: f64,
    },
}

pub(crate) fn evaluate(
    config: &MachineWarningsConfig,
    machine: &MachineSnapshot,
    storage: &StorageSnapshot,
) -> Vec<MachineWarning> {
    if !config.enabled {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    let supplying_kind = match machine.power.source {
        PowerSource::Battery => Some(BatteryKind::System),
        PowerSource::Ups => Some(BatteryKind::Ups),
        PowerSource::External | PowerSource::Unknown => None,
    };
    if machine.power.external_power != Some(true) {
        for battery in machine.power.batteries.iter().flatten() {
            if Some(battery.kind) == supplying_kind
                && battery.present != Some(false)
                && battery.state == BatteryState::Discharging
                && let Some(charge_percent) = battery.charge_percent
                && charge_percent <= config.battery_percent
            {
                warnings.push(MachineWarning::LowBattery {
                    name: battery.name.clone(),
                    battery_kind: battery.kind,
                    charge_percent,
                });
            }
        }
    }
    for filesystem in &storage.filesystems {
        if !filesystem.targets.is_empty()
            && let Some(available_percent) = filesystem.available_percent()
            && u128::from(filesystem.available_bytes) * 100
                <= u128::from(filesystem.total_bytes) * u128::from(config.disk_free_percent)
        {
            warnings.push(MachineWarning::LowDiskSpace {
                filesystem: filesystem.id,
                targets: filesystem
                    .targets
                    .iter()
                    .map(|entry| entry.target.clone())
                    .collect(),
                available_bytes: filesystem.available_bytes,
                available_percent,
            });
        }
    }
    if config.thermal {
        if matches!(
            machine.thermal.state,
            ThermalState::Warning | ThermalState::Critical
        ) {
            warnings.push(MachineWarning::Thermal {
                state: machine.thermal.state,
            });
        }
        for sensor in &machine.thermal.cpu_temperatures {
            // A custom physical-temperature threshold is not an AMD Tctl threshold.
            let configured = config
                .cpu_temperature_celsius
                .filter(|_| sensor.kind == TemperatureKind::Physical);
            let threshold = sensor
                .critical_celsius
                .into_iter()
                .chain(configured)
                .filter(|value| value.is_finite())
                .min_by(f64::total_cmp);
            if let (Some(value), Some(threshold)) = (sensor.celsius, threshold)
                && value.is_finite()
                && value >= threshold
            {
                warnings.push(MachineWarning::CpuTemperature {
                    sensor: sensor.id.clone(),
                    temperature_kind: sensor.kind,
                    value,
                    threshold,
                });
            }
        }
    }
    warnings
}

/// Only fixed vocabulary and measured numbers enter developer instructions.
/// OS-provided names and paths stay in the resource as data, not prompt text.
pub(crate) fn instructions(warnings: &[MachineWarning]) -> Option<String> {
    if warnings.is_empty() {
        return None;
    }
    let mut facts = Vec::new();
    let mut actions = Vec::new();
    for warning in warnings {
        let action = match warning {
            MachineWarning::LowBattery {
                battery_kind,
                charge_percent,
                ..
            } => {
                let kind = match battery_kind {
                    BatteryKind::System => "system battery",
                    BatteryKind::Ups => "UPS battery",
                };
                facts.push(format!(
                    "Discharging {kind}: {charge_percent}% (not combined capacity)."
                ));
                "restore external power"
            }
            MachineWarning::LowDiskSpace {
                targets,
                available_percent,
                ..
            } => {
                let mut roles = Vec::new();
                for target in targets {
                    let role = match target.role {
                        StorageRole::Workspace => "workspace",
                        StorageRole::State => "state",
                        StorageRole::Temporary => "temporary",
                        StorageRole::Output => "output",
                        StorageRole::Checkpoint => "checkpoint",
                    };
                    if !roles.contains(&role) {
                        roles.push(role);
                    }
                }
                facts.push(format!(
                    "Disk ({}): {available_percent:.1}% available.",
                    roles.join("/")
                ));
                "make storage space available"
            }
            MachineWarning::Thermal { state } => {
                let state = if *state == ThermalState::Critical {
                    "critical"
                } else {
                    "warning"
                };
                facts.push(format!("OS thermal state: {state}."));
                "check cooling"
            }
            MachineWarning::CpuTemperature { .. } => {
                if !facts
                    .iter()
                    .any(|fact| fact == "CPU thermal threshold reached.")
                {
                    facts.push("CPU thermal threshold reached.".into());
                }
                "check cooling"
            }
        };
        if !actions.contains(&action) {
            actions.push(action);
        }
    }
    Some(format!(
        "Machine warning (harness host): {}\n\
         Save essential work and a minimal checkpoint to verified persistent storage outside /tmp and other temporary directories; \
         verify the write succeeded. If unavailable, tell the operator. Do not rely on suspend to preserve data. \
         Pause heavy or interruption-sensitive work, including flashing. \
         Tell the operator to {}; avoid repeating unchanged notifications. \
         Do not delete files or suspend the machine automatically.",
        facts.join(" "),
        actions.join(", "),
    ))
}

#[cfg(test)]
mod tests;
