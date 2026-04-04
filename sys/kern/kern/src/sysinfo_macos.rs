//! macOS-specific system fact detection.
//!
//! Gathers information from `sysctl`, `uname`, IOKit (via
//! `core-foundation`), and `pmset`.

use super::{
    SandboxKind, SystemInfo, detect_disk, detect_display_server, detect_hostname, detect_locale,
    detect_os_version, detect_shell, detect_timezone,
};
use std::ffi::CStr;
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
    }
}

// ── sysctl helpers ───────────────────────────────────────────────────

fn sysctl_u64(name: &str) -> Option<u64> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let mut value: u64 = 0;
    let mut size = mem::size_of::<u64>();
    let ret = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
            &mut value as *mut u64 as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 { Some(value) } else { None }
}

fn sysctl_u32(name: &str) -> Option<u32> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let mut value: u32 = 0;
    let mut size = mem::size_of::<u32>();
    let ret = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
            &mut value as *mut u32 as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 { Some(value) } else { None }
}

fn sysctl_string(name: &str) -> Option<String> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let mut size: usize = 0;

    // First call to get the size
    let ret = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 || size == 0 {
        return None;
    }

    let mut buf = vec![0u8; size];
    let ret = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 {
        return None;
    }

    // buf contains a NUL-terminated C string
    let c_str = unsafe { CStr::from_ptr(buf.as_ptr() as *const libc::c_char) };
    Some(c_str.to_string_lossy().into_owned())
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
    let c_name = std::ffi::CString::new("kern.boottime").unwrap();
    let mut tv: libc::timeval = unsafe { mem::zeroed() };
    let mut size = mem::size_of::<libc::timeval>();
    let ret = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
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

fn detect_power() -> (bool, Option<u8>, bool) {
    // Use `pmset -g batt` for battery info — simpler than IOKit bindings.
    let Ok(output) = std::process::Command::new("pmset")
        .args(["-g", "batt"])
        .output()
    else {
        return (false, None, false);
    };

    let text = String::from_utf8_lossy(&output.stdout);

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
