use super::text;
use crate::thermal::valid_celsius;
use crate::{CpuTemperature, TemperatureKind, TemperatureSource, ThermalInfo};
use std::fs::{self, DirEntry};
use std::io;
use std::path::Path;

pub(crate) fn read() -> ThermalInfo {
    read_at(Path::new("/"))
}

fn read_at(root: &Path) -> ThermalInfo {
    let mut cpu_temperatures = Vec::new();
    for device in entries(&root.join("sys/class/hwmon")) {
        let path = device.path();
        let Some(driver) = text(&path.join("name")) else {
            continue;
        };
        // Never mistake an NVMe, GPU, or unclassified motherboard sensor for
        // the CPU merely because it is the first/hottest hwmon channel.
        if !matches!(
            driver.as_str(),
            "coretemp" | "k10temp" | "k8temp" | "via_cputemp"
        ) {
            continue;
        }
        for input in entries(&path) {
            let name = input.file_name();
            let Some(channel) =
                index(&name.to_string_lossy(), "temp", "_input").filter(|channel| *channel > 0)
            else {
                continue;
            };
            let prefix = format!("temp{channel}");
            let label = text(&path.join(format!("{prefix}_label")))
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| prefix.clone());
            let usable = optional_flag(&path.join(format!("{prefix}_enable")), "1")
                && optional_flag(&path.join(format!("{prefix}_fault")), "0");
            cpu_temperatures.push(CpuTemperature {
                id: input.path().to_string_lossy().into_owned(),
                label: format!("{driver}: {label}"),
                source: TemperatureSource::Hwmon,
                // k10temp's first channel is Tctl even on drivers without labels.
                kind: if driver == "k10temp" && channel == 1 {
                    TemperatureKind::Control
                } else {
                    TemperatureKind::Physical
                },
                celsius: usable.then(|| millidegrees(&input.path())).flatten(),
                critical_celsius: millidegrees(&path.join(format!("{prefix}_crit"))),
            });
        }
    }
    for zone in entries(&root.join("sys/class/thermal")) {
        if index(&zone.file_name().to_string_lossy(), "thermal_zone", "").is_none() {
            continue;
        }
        let path = zone.path();
        let Some(label) = text(&path.join("type")).filter(|name| cpu_zone(name)) else {
            continue;
        };
        let critical_celsius = entries(&path)
            .into_iter()
            .filter_map(|entry| {
                let name = entry.file_name();
                let trip = index(&name.to_string_lossy(), "trip_point_", "_type")?;
                (text(&entry.path()).as_deref() == Some("critical"))
                    .then(|| millidegrees(&path.join(format!("trip_point_{trip}_temp"))))
                    .flatten()
            })
            .min_by(f64::total_cmp);
        let input = path.join("temp");
        cpu_temperatures.push(CpuTemperature {
            id: input.to_string_lossy().into_owned(),
            label,
            source: TemperatureSource::ThermalZone,
            kind: TemperatureKind::Physical,
            // Zone `mode` governs the kernel control loop, not sensor validity.
            celsius: millidegrees(&input),
            critical_celsius,
        });
    }
    ThermalInfo {
        cpu_temperatures,
        ..ThermalInfo::default()
    }
}

fn entries(path: &Path) -> Vec<DirEntry> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(DirEntry::file_name);
    entries
}

fn index(name: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let number = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

fn cpu_zone(name: &str) -> bool {
    if matches!(name, "x86_pkg_temp" | "cpu" | "cpu-thermal" | "cpu_thermal") {
        return true;
    }
    ["", "-thermal", "_thermal"]
        .iter()
        .any(|suffix| index(name, "cpu", suffix).is_some())
}

fn millidegrees(path: &Path) -> Option<f64> {
    let value: i32 = text(path)?.parse().ok()?;
    valid_celsius(f64::from(value) / 1000.0)
}

fn optional_flag(path: &Path, expected: &str) -> bool {
    match fs::read_to_string(path) {
        Ok(value) => value.trim() == expected,
        // These attributes are optional. Unreadable or malformed is not absent.
        Err(error) => error.kind() == io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
mod tests;
