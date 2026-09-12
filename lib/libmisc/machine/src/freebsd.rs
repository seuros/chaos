use crate::power::source_from_supplies;
use crate::{BatteryInfo, BatteryKind, BatteryState, PowerInfo};

mod thermal;
#[cfg(target_os = "freebsd")]
pub(crate) use thermal::read as read_thermal;

#[cfg(target_os = "freebsd")]
pub(crate) fn detect() -> (crate::MachineProfile, PowerInfo) {
    use crate::sysctl::{integer, string};
    use crate::{ExecutionEnvironment, MachineProfile};

    if integer(c"security.jail.jailed") == Some(1) {
        return (
            MachineProfile {
                execution_environment: ExecutionEnvironment::Container,
                ..MachineProfile::default()
            },
            PowerInfo::default(),
        );
    }
    let execution_environment = match string(c"kern.vm_guest").as_deref() {
        Some("none") => ExecutionEnvironment::NoneDetected,
        Some(_) => ExecutionEnvironment::VirtualMachine,
        None => ExecutionEnvironment::Unknown,
    };
    let power = from_acpi(
        integer(c"hw.acpi.acline"),
        integer(c"hw.acpi.battery.units"),
        integer(c"hw.acpi.battery.life"),
        integer(c"hw.acpi.battery.state"),
    );
    let mut profile = MachineProfile {
        execution_environment,
        ..MachineProfile::default()
    };
    if execution_environment != ExecutionEnvironment::VirtualMachine {
        (profile.form_factor, profile.form_factor_evidence) =
            crate::profile::classify(None, None, &power);
    }
    (profile, power)
}

fn from_acpi(
    ac: Option<i32>,
    units: Option<i32>,
    life: Option<i32>,
    state: Option<i32>,
) -> PowerInfo {
    let external_power = match ac {
        Some(0) => Some(false),
        Some(1) => Some(true),
        _ => None,
    };
    let batteries = match units {
        Some(0) => Some(Vec::new()),
        Some(1..) => {
            // FreeBSD's life/state sysctls describe the aggregate system battery.
            let present = match state {
                Some(7) => Some(false), // ACPI_BATT_STAT_NOT_PRESENT
                Some(0..=6) => Some(true),
                _ => None,
            };
            let charge_percent = if present == Some(false) {
                None
            } else {
                life.filter(|value| (0..=100).contains(value))
                    .map(|value| value as u8)
            };
            let state = match state {
                Some(1 | 5) => BatteryState::Discharging,
                Some(2 | 6) => BatteryState::Charging,
                Some(0 | 4) => BatteryState::Idle,
                _ => BatteryState::Unknown,
            };
            Some(vec![BatteryInfo {
                name: "system".into(),
                kind: BatteryKind::System,
                present,
                charge_percent,
                state,
            }])
        }
        _ => None,
    };
    PowerInfo {
        source: source_from_supplies(external_power, batteries.as_deref()),
        external_power,
        batteries,
    }
}

#[cfg(test)]
mod tests;
