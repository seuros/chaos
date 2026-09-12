use crate::thermal::valid_celsius;
use crate::{CpuTemperature, TemperatureKind, TemperatureSource, ThermalInfo};
use std::ffi::{CStr, CString};

#[cfg(target_os = "freebsd")]
pub(crate) fn read() -> ThermalInfo {
    read_with(crate::sysctl::integer)
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

#[cfg(test)]
mod tests;
