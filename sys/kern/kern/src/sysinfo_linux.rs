//! Linux-specific system fact detection.
//!
//! Gathers information from `/proc`, `/sys`, `/etc/os-release`, and libc
//! syscalls.  No external crates beyond `libc`.

use super::{
    SandboxKind, SystemInfo, detect_disk, detect_display_server, detect_has_network,
    detect_hostname, detect_locale, detect_os_version, detect_shell, detect_timezone,
};
use std::fs;
use std::path::Path;

/// Construct a fully-populated [`SystemInfo`] for this Linux host.
pub(super) fn detect() -> SystemInfo {
    let (mem_total, mem_available) = detect_memory();
    let (disk_total, disk_available) = detect_disk();
    let (has_battery, battery_level, charger_connected) = detect_power();
    let (in_container, container_type) = detect_container();

    SystemInfo {
        os: "linux".into(),
        os_version: detect_os_version(),
        os_distro: detect_distro(),
        arch: std::env::consts::ARCH.into(),
        hostname: detect_hostname(),

        memory_total: mem_total,
        memory_available: mem_available,
        cpu_cores: detect_cpu_cores(),
        cpu_model: detect_cpu_model(),
        disk_total,
        disk_available,

        has_battery,
        battery_level,
        charger_connected,

        uptime_secs: detect_uptime(),
        sandbox_type: SandboxKind::for_platform(),
        in_container,
        container_type,
        shell: detect_shell(),
        display_server: detect_display_server(),
        locale: detect_locale(),
        timezone: detect_timezone(),
        has_network: detect_has_network(),
    }
}

// ── Memory ───────────────────────────────────────────────────────────

fn detect_memory() -> (u64, u64) {
    let Ok(meminfo) = fs::read_to_string("/proc/meminfo") else {
        return (0, 0);
    };
    let mut total: u64 = 0;
    let mut available: u64 = 0;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = parse_meminfo_kb(rest) * 1024;
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = parse_meminfo_kb(rest) * 1024;
        }
        if total > 0 && available > 0 {
            break;
        }
    }
    (total, available)
}

fn parse_meminfo_kb(s: &str) -> u64 {
    s.split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

// ── CPU ──────────────────────────────────────────────────────────────

fn detect_cpu_cores() -> u32 {
    // sysconf is the reliable way
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n > 0 { n as u32 } else { 1 }
}

fn detect_cpu_model() -> String {
    let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") else {
        return "unknown".into();
    };
    for line in cpuinfo.lines() {
        if let Some(rest) = line.strip_prefix("model name") {
            if let Some(name) = rest.strip_prefix('\t').and_then(|s| s.strip_prefix(": ")) {
                return name.trim().into();
            }
            // handle "model name\t: ..." without leading tab normalization
            if let Some(pos) = rest.find(':') {
                return rest[pos + 1..].trim().into();
            }
        }
    }
    "unknown".into()
}

// ── Uptime ───────────────────────────────────────────────────────────

fn detect_uptime() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .map(|v| v as u64)
        .unwrap_or(0)
}

// ── Power / Battery ──────────────────────────────────────────────────

fn detect_power() -> (bool, Option<u8>, bool) {
    let supply = Path::new("/sys/class/power_supply");
    if !supply.exists() {
        return (false, None, false);
    }

    let mut has_battery = false;
    let mut battery_level: Option<u8> = None;
    let mut charger_connected = false;

    let Ok(entries) = fs::read_dir(supply) else {
        return (false, None, false);
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let type_path = path.join("type");
        let Ok(supply_type) = fs::read_to_string(&type_path) else {
            continue;
        };
        let supply_type = supply_type.trim();

        match supply_type {
            "Battery" => {
                has_battery = true;
                if battery_level.is_none() {
                    battery_level = fs::read_to_string(path.join("capacity"))
                        .ok()
                        .and_then(|s| s.trim().parse().ok());
                }
            }
            "Mains" | "USB" => {
                let online = fs::read_to_string(path.join("online"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u8>().ok())
                    .unwrap_or(0);
                if online == 1 {
                    charger_connected = true;
                }
            }
            _ => {}
        }
    }

    (has_battery, battery_level, charger_connected)
}

// ── Distro ───────────────────────────────────────────────────────────

fn detect_distro() -> String {
    // Prefer /etc/os-release ID field
    if let Ok(content) = fs::read_to_string("/etc/os-release") {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("ID=") {
                return rest.trim_matches('"').to_lowercase();
            }
        }
    }
    // Fallback to marker files
    if Path::new("/etc/arch-release").exists() {
        "arch".into()
    } else if Path::new("/etc/debian_version").exists() {
        "debian".into()
    } else if Path::new("/etc/alpine-release").exists() {
        "alpine".into()
    } else if Path::new("/etc/fedora-release").exists() {
        "fedora".into()
    } else {
        String::new()
    }
}

// ── Container Detection ──────────────────────────────────────────────

fn detect_container() -> (bool, String) {
    // /.dockerenv
    if Path::new("/.dockerenv").exists() {
        return (true, "docker".into());
    }

    // $container env (set by systemd-nspawn, podman)
    if let Ok(val) = std::env::var("container") {
        return (true, val);
    }

    // cgroup heuristics
    if let Ok(cgroup) = fs::read_to_string("/proc/1/cgroup") {
        if cgroup.contains("docker") {
            return (true, "docker".into());
        }
        if cgroup.contains("podman") {
            return (true, "podman".into());
        }
        if cgroup.contains("lxc") {
            return (true, "lxc".into());
        }
        if cgroup.contains("kubepods") {
            return (true, "kubernetes".into());
        }
    }

    (false, String::new())
}
