//! FreeBSD-specific system fact detection.
//!
//! Gathers information from `sysctl`, `uname`, and the jail subsystem.

use super::{
    SandboxKind, SystemInfo, detect_disk, detect_display_server, detect_hostname, detect_locale,
    detect_multiplexer, detect_os_version, detect_shell, detect_timezone,
};
use std::ffi::CStr;
use std::mem;

/// Construct a fully-populated [`SystemInfo`] for this FreeBSD host.
pub(super) fn detect() -> SystemInfo {
    let (disk_total, disk_available) = detect_disk();
    let (has_battery, battery_level, charger_connected) = detect_power();
    let (in_container, container_type) = detect_jail();

    SystemInfo {
        os: "freebsd".into(),
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
        in_container,
        container_type,
        shell: detect_shell(),
        display_server: detect_display_server(),
        locale: detect_locale(),
        timezone: detect_timezone(),
        has_network: super::detect_has_network(),
        multiplexer: detect_multiplexer(),
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

    let c_str = unsafe { CStr::from_ptr(buf.as_ptr() as *const libc::c_char) };
    Some(c_str.to_string_lossy().into_owned())
}

// ── Memory ───────────────────────────────────────────────────────────

fn detect_memory_total() -> u64 {
    sysctl_u64("hw.physmem").unwrap_or(0)
}

fn detect_memory_available() -> u64 {
    // Free + inactive pages
    let page_size = sysctl_u64("hw.pagesize").unwrap_or(4096);
    let free = sysctl_u32("vm.stats.vm.v_free_count").unwrap_or(0) as u64;
    let inactive = sysctl_u32("vm.stats.vm.v_inactive_count").unwrap_or(0) as u64;
    (free + inactive) * page_size
}

// ── CPU ──────────────────────────────────────────────────────────────

fn detect_cpu_cores() -> u32 {
    sysctl_u32("hw.ncpu").unwrap_or(1)
}

fn detect_cpu_model() -> String {
    sysctl_string("hw.model").unwrap_or_else(|| "unknown".into())
}

// ── Uptime ───────────────────────────────────────────────────────────

fn detect_uptime() -> u64 {
    let Some(c_name) = std::ffi::CString::new("kern.boottime").ok() else {
        return 0;
    };
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
    // hw.acpi.battery.life — percentage
    // hw.acpi.battery.state — 0=not charging, 1=discharging, 2=charging
    // hw.acpi.acline — 1=on AC, 0=on battery
    let life = sysctl_u32("hw.acpi.battery.life");
    let acline = sysctl_u32("hw.acpi.acline");

    match life {
        Some(pct) => {
            let charger = acline.unwrap_or(0) == 1;
            (true, Some(pct.min(100) as u8), charger)
        }
        None => (false, None, false),
    }
}

// ── Jail Detection ───────────────────────────────────────────────────

fn detect_jail() -> (bool, String) {
    // security.jail.jailed sysctl is 1 inside a jail
    match sysctl_u32("security.jail.jailed") {
        Some(1) => (true, "jail".into()),
        _ => (false, String::new()),
    }
}
