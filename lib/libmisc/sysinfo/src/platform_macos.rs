//! macOS-specific system fact detection.
//!
//! Gathers information from `sysctl`, `uname`, IOKit (via
//! `core-foundation`), and `pmset`.

use super::sysctl::{sysctl_string, sysctl_u32, sysctl_u64};
use super::{
    SandboxKind, SystemInfo, detect_disk, detect_display_server, detect_hostname, detect_locale,
    detect_multiplexer, detect_os_version, detect_shell, detect_timezone,
};
use std::mem;

/// Construct a fully-populated [`SystemInfo`] for this macOS host.
pub(super) fn detect() -> SystemInfo {
    let (disk_total, disk_available) = detect_disk();
    let (has_battery, battery_level, charger_connected) = detect_power();

    SystemInfo {
        os: "macos".into(),
        os_version: detect_os_version(),
        os_distro: String::new(), // not applicable
        arch: std::env::consts::ARCH.into(),
        hostname: detect_hostname(),

        memory_total: detect_memory_total(),
        memory_available: detect_memory_available(),
        cpu_cores: detect_cpu_cores(),
        cpu_model: detect_cpu_model(),
        disk_total,
        disk_available,

        has_battery,
        battery_level,
        charger_connected,

        uptime_secs: detect_uptime(),
        sandbox_type: SandboxKind::for_platform(),
        in_container: false, // macOS doesn't run in containers
        container_type: String::new(),
        shell: detect_shell(),
        display_server: detect_display_server(),
        locale: detect_locale(),
        timezone: detect_timezone(),
        has_network: super::detect_has_network(),
        multiplexer: detect_multiplexer(),
    }
}

// ── Memory ───────────────────────────────────────────────────────────

fn detect_memory_total() -> u64 {
    sysctl_u64("hw.memsize").unwrap_or(0)
}

fn detect_memory_available() -> u64 {
    // macOS doesn't expose "available" directly via sysctl.
    // Use vm.page_pageable_internal_count + vm.page_purgeable_count as
    // a rough proxy, or fall back to vm_statistics.  For simplicity we
    // use host_statistics64 via mach.
    //
    // Simpler fallback: read free + inactive pages from vm.page counts.
    let page_size = sysctl_u64("hw.pagesize").unwrap_or(4096);
    // vm.page_free_count gives free pages
    let free_pages = sysctl_u64("vm.page_free_count").unwrap_or(0);
    // Inactive pages can be reclaimed
    let inactive = sysctl_u64("vm.page_inactive_count").unwrap_or(0);
    (free_pages + inactive) * page_size
}

// ── CPU ──────────────────────────────────────────────────────────────

fn detect_cpu_cores() -> u32 {
    sysctl_u32("hw.logicalcpu").unwrap_or(1)
}

fn detect_cpu_model() -> String {
    sysctl_string("machdep.cpu.brand_string").unwrap_or_else(|| "unknown".into())
}

// ── Uptime ───────────────────────────────────────────────────────────

fn detect_uptime() -> u64 {
    let mut tv: libc::timeval = unsafe { mem::zeroed() };
    let mut size = mem::size_of::<libc::timeval>();
    let ret = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            &mut tv as *mut libc::timeval as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 {
        let mut now: libc::timeval = unsafe { mem::zeroed() };
        unsafe { libc::gettimeofday(&mut now, std::ptr::null_mut()) };
        (now.tv_sec - tv.tv_sec).max(0) as u64
    } else {
        0
    }
}

// ── Power / Battery ──────────────────────────────────────────────────

pub(super) fn detect_power() -> (bool, Option<u8>, bool) {
    // Use `pmset -g batt` for battery info — simpler than IOKit bindings.
    let Ok(mut child) = std::process::Command::new("/usr/bin/pmset")
        .args(["-g", "batt"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return (false, None, false);
    };

    // A background refresh must not leave an unbounded command behind.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return (false, None, false);
            }
        }
    }
    let Ok(output) = child.wait_with_output() else {
        return (false, None, false);
    };
    if !output.status.success() {
        return (false, None, false);
    }
    parse_power(&String::from_utf8_lossy(&output.stdout))
}

fn parse_power(text: &str) -> (bool, Option<u8>, bool) {
    // No battery on desktops
    if !text.contains("Battery") && !text.contains("InternalBattery") {
        return (false, None, false);
    }

    let has_battery = true;
    let charger_connected = text.contains("AC Power");

    // Parse percentage from line like "InternalBattery-0 (id=...)	85%; charging;"
    let battery_level = text
        .lines()
        .find(|l| l.contains("InternalBattery"))
        .and_then(|line| {
            line.split_whitespace()
                .find(|w| w.ends_with("%;"))
                .or_else(|| line.split_whitespace().find(|w| w.ends_with('%')))
                .and_then(|w| w.trim_end_matches(&['%', ';'][..]).parse().ok())
        });

    (has_battery, battery_level, charger_connected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_output_distinguishes_charge_unknown_and_absent_battery() {
        assert_eq!(
            parse_power("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t85%; charging;"),
            (true, Some(85), true)
        );
        assert_eq!(
            parse_power(
                "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t15%; discharging;"
            ),
            (true, Some(15), false)
        );
        assert_eq!(
            parse_power("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\tunknown;"),
            (true, None, true)
        );
        assert_eq!(
            parse_power("Now drawing from 'AC Power'"),
            (false, None, false)
        );
    }
}
