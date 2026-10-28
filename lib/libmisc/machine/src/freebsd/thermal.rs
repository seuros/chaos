use crate::thermal::valid_celsius;
use crate::{CpuTemperature, TemperatureKind, TemperatureSource, ThermalInfo, ThermalState};
use std::ffi::{CStr, CString};

#[cfg(target_os = "freebsd")]
pub(crate) fn read() -> ThermalInfo {
    // Do not leak host observations if jail detection itself is inaccessible.
    if crate::sysctl::integer(c"security.jail.jailed") != Some(0) {
        return ThermalInfo::default();
    }
    let mut thermal = read_with(crate::sysctl::integer);
    let names = crate::sysctl::names(c"hw.acpi.thermal", 4096);
    thermal.state = system_state(names.as_deref(), crate::sysctl::integer);
    thermal
}

fn read_with(integer: impl Fn(&CStr) -> Option<i32>) -> ThermalInfo {
    let Some(count) = integer(c"hw.ncpu").filter(|count| (1..=4096).contains(count)) else {
        return ThermalInfo::default();
    };
    let mut cpu_temperatures = Vec::new();
    for cpu in 0..count {
        let id = format!("dev.cpu.{cpu}.temperature");
        let Ok(name) = CString::new(id.as_str()) else {
            continue;
        };
        let Some(value) = integer(&name) else {
            // No sensor sysctl for this logical CPU. Do not stop at a gap.
            continue;
        };
        let critical_celsius = CString::new(format!("dev.cpu.{cpu}.coretemp.tjmax"))
            .ok()
            .as_deref()
            .and_then(&integer)
            .and_then(decikelvin);
        cpu_temperatures.push(CpuTemperature {
            id,
            label: format!("CPU {cpu}"),
            source: TemperatureSource::Sysctl,
            // The generic CPU sysctl alone does not identify the sensor driver.
            kind: TemperatureKind::Unknown,
            celsius: decikelvin(value),
            critical_celsius,
        });
    }
    ThermalInfo {
        cpu_temperatures,
        ..ThermalInfo::default()
    }
}

fn decikelvin(value: i32) -> Option<f64> {
    valid_celsius(f64::from(value) / 10.0 - 273.15)
}

fn system_state(names: Option<&[CString]>, integer: impl Fn(&CStr) -> Option<i32>) -> ThermalState {
    let Some(names) = names else {
        return ThermalState::Unknown;
    };
    let mut observed = false;
    let mut complete = true;
    let mut warning = false;
    for name in names {
        let Some(zone) = name
            .to_str()
            .ok()
            .and_then(|name| name.strip_prefix("hw.acpi.thermal.tz"))
            .and_then(|name| name.strip_suffix(".thermal_flags"))
            .filter(|number| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            })
        else {
            continue;
        };
        observed = true;
        let flags = integer(name);
        let state = match flags {
            // acpi_thermal.c's TZ_THFLAG_CR3/HOT/CRT mean standby, suspend,
            // and shutdown. PSV is OS-requested passive throttling.
            Some(flags) if flags >= 0 && flags & 0x1c != 0 => ThermalState::Critical,
            Some(flags) if flags >= 0 && flags & 1 != 0 => ThermalState::Warning,
            Some(0) => {
                let read = |suffix| {
                    CString::new(format!("hw.acpi.thermal.tz{zone}.{suffix}"))
                        .ok()
                        .as_deref()
                        .and_then(&integer)
                        .and_then(decikelvin)
                };
                // An initialized zero flag with no working sensor/trip points
                // is not evidence that the OS supports thermal warnings.
                if read("temperature").is_some()
                    && ["_PSV", "_CR3", "_HOT", "_CRT"]
                        .iter()
                        .any(|suffix| read(suffix).is_some())
                {
                    ThermalState::Normal
                } else {
                    ThermalState::Unknown
                }
            }
            _ => ThermalState::Unknown,
        };
        match state {
            ThermalState::Critical => return ThermalState::Critical,
            ThermalState::Warning => warning = true,
            ThermalState::Unknown => complete = false,
            ThermalState::Normal => {}
        }
    }
    if warning {
        ThermalState::Warning
    } else if observed && complete {
        ThermalState::Normal
    } else {
        ThermalState::Unknown
    }
}

#[cfg(test)]
mod tests;
