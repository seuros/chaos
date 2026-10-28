//! Warning identity across observations, independent of changing readings.

use crate::machine_warnings::MachineWarning;

pub(super) fn same_condition(a: &MachineWarning, b: &MachineWarning) -> bool {
    match (a, b) {
        (
            MachineWarning::LowBattery {
                name: a,
                battery_kind: ak,
                ..
            },
            MachineWarning::LowBattery {
                name: b,
                battery_kind: bk,
                ..
            },
        ) => a == b && ak == bk,
        (
            MachineWarning::LowDiskSpace { filesystem: a, .. },
            MachineWarning::LowDiskSpace { filesystem: b, .. },
        ) => a == b,
        (MachineWarning::Thermal { .. }, MachineWarning::Thermal { .. }) => true,
        (
            MachineWarning::CpuTemperature {
                sensor: a,
                temperature_kind: ak,
                ..
            },
            MachineWarning::CpuTemperature {
                sensor: b,
                temperature_kind: bk,
                ..
            },
        ) => a == b && ak == bk,
        _ => false,
    }
}
