use crate::power::source_from_supplies;
use crate::{BatteryInfo, BatteryKind, BatteryState, PowerInfo};

#[cfg(target_os = "freebsd")]
pub(super) fn read() -> PowerInfo {
    use super::abi;
    use std::os::fd::AsRawFd;

    let batteries = (|| {
        // Battery information ioctls are allowed on a read-only ACPI fd.
        let file = std::fs::File::open("/dev/acpi").ok()?;
        let mut count = -1_i32;
        // SAFETY: BATTERY_UNITS has one writable int output.
        unsafe { abi::query(file.as_raw_fd(), abi::BATTERY_UNITS, &mut count) }.ok()?;
        let batteries = collect(Some(count), |unit| {
            let mut argument = abi::BatteryArgument { storage: [0; 64] };
            argument.unit = unit;
            // SAFETY: this is the complete ioctl union, initialized with its unit.
            unsafe { abi::query(file.as_raw_fd(), abi::BATTERY_INFO, &mut argument) }.ok()?;
            // SAFETY: a successful GET_BATTINFO initializes the info member.
            let info = unsafe { argument.info };
            Some((info.cap, info.state))
        })?;
        let mut after = -1_i32;
        // SAFETY: same int-output query. A changing inventory is not complete.
        unsafe { abi::query(file.as_raw_fd(), abi::BATTERY_UNITS, &mut after) }.ok()?;
        (count == after).then_some(batteries)
    })();
    from_supplies(crate::sysctl::integer(c"hw.acpi.acline"), batteries)
}

fn collect(
    count: Option<i32>,
    mut query: impl FnMut(i32) -> Option<(i32, i32)>,
) -> Option<Vec<BatteryInfo>> {
    let count = count.filter(|count| (0..=128).contains(count))?;
    (0..count)
        .map(|unit| {
            let (capacity, flags) = query(unit)?;
            let (present, state) = match flags {
                7 => (Some(false), BatteryState::Unknown), // NOT_PRESENT, not a bit to mask
                1 | 5 => (Some(true), BatteryState::Discharging),
                2 | 6 => (Some(true), BatteryState::Charging),
                0 | 4 => (Some(true), BatteryState::Idle),
                _ => (None, BatteryState::Unknown), // contradictory or unknown flags
            };
            Some(BatteryInfo {
                name: format!("acpi{unit}"),
                kind: BatteryKind::System,
                present,
                charge_percent: (present == Some(true) && (0..=100).contains(&capacity))
                    .then_some(capacity as u8),
                state,
            })
        })
        .collect()
}

fn from_supplies(ac: Option<i32>, batteries: Option<Vec<BatteryInfo>>) -> PowerInfo {
    let external_power = match ac {
        Some(0) => Some(false),
        Some(1) => Some(true),
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
