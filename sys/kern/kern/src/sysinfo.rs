//! Centralized system information.
//!
//! Provides a single [`SystemInfo`] struct with OS, hardware, power, and
//! runtime information gathered once at startup and cached for the lifetime
//! of the process.  Platform-specific detection lives in separate files
//! (`sysinfo_linux.rs`, `sysinfo_macos.rs`, `sysinfo_freebsd.rs`) that each
//! implement the same `detect() -> SystemInfo` entry point.

use serde::Serialize;
use std::sync::OnceLock;

#[cfg(target_os = "linux")]
#[path = "sysinfo_linux.rs"]
mod platform;

#[cfg(target_os = "macos")]
#[path = "sysinfo_macos.rs"]
mod platform;

#[cfg(target_os = "freebsd")]
#[path = "sysinfo_freebsd.rs"]
mod platform;

// ── Public API ───────────────────────────────────────────────────────

static SYSINFO: OnceLock<SystemInfo> = OnceLock::new();

/// Returns the cached system info, computing it on first call.
pub fn sysinfo() -> &'static SystemInfo {
    SYSINFO.get_or_init(platform::detect)
}

// ── Struct ───────────────────────────────────────────────────────────

/// Snapshot of the host environment, collected once at startup.
///
/// Every field is best-effort.  String fields default to `"unknown"` and
/// numeric fields default to `0` when detection fails.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct SystemInfo {
    // ── Core ─────────────────────────────────────────────────────
    /// Canonical OS name: `"linux"`, `"macos"`, `"freebsd"`.
    pub os: String,
    /// Kernel / OS version string (e.g. `"6.19.8"`, `"24.5.0"`).
    pub os_version: String,
    /// Linux distro id from `/etc/os-release` (e.g. `"arch"`, `"debian"`).
    /// Empty on non-Linux.
    pub os_distro: String,
    /// CPU architecture: `"x86_64"`, `"aarch64"`, etc.
    pub arch: String,
    /// Machine hostname.
    pub hostname: String,

    // ── Hardware ─────────────────────────────────────────────────
    /// Total physical RAM in bytes.
    pub memory_total: u64,
    /// Available (free + reclaimable) RAM in bytes.
    pub memory_available: u64,
    /// Logical CPU core count.
    pub cpu_cores: u32,
    /// CPU model string (e.g. `"AMD Ryzen 9 7950X"`).
    pub cpu_model: String,
    /// Total size of the working partition in bytes.
    pub disk_total: u64,
    /// Free space on the working partition in bytes.
    pub disk_available: u64,

    // ── Power ────────────────────────────────────────────────────
    /// Whether the machine has a battery (laptop heuristic).
    pub has_battery: bool,
    /// Battery charge percentage 0–100, `None` if no battery.
    pub battery_level: Option<u8>,
    /// Whether AC power / charger is connected.
    pub charger_connected: bool,

    // ── Runtime ──────────────────────────────────────────────────
    /// Seconds since boot.
    pub uptime_secs: u64,
    /// Platform sandbox mechanism.
    pub sandbox_type: SandboxKind,
    /// Whether running inside a container / jail.
    pub in_container: bool,
    /// Container runtime if detected: `"docker"`, `"podman"`, `"jail"`, etc.
    pub container_type: String,
    /// User's login shell from `$SHELL`.
    pub shell: String,
    /// Display server: `"wayland"`, `"x11"`, `"aqua"`, `"none"`.
    pub display_server: String,
    /// Locale from `$LANG`.
    pub locale: String,
    /// IANA timezone.
    pub timezone: String,
    /// Whether any non-loopback interface with an IPv4/IPv6 address is up.
    pub has_network: bool,

    // ── Terminal ─────────────────────────────────────────────────
    /// Terminal multiplexer info, if running inside tmux/zellij/screen.
    pub multiplexer: Option<MultiplexerInfo>,
}

/// Terminal multiplexer session info (tmux, zellij, screen).
///
/// Provides enough context to address panes by coordinate, e.g. `0:1.2`
/// means session `0`, window `1`, pane `2`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MultiplexerInfo {
    /// `"tmux"`, `"zellij"`, or `"screen"`.
    pub kind: String,
    /// Session name / id.
    pub session: String,
    /// Window index (tmux) or tab name — empty when not applicable.
    pub window: String,
    /// Pane index within the window.
    pub pane: String,
}

/// Platform sandbox mechanism available at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxKind {
    None,
    Seatbelt,
    Seccomp,
    Capsicum,
}

impl SandboxKind {
    /// Compile-time detection based on `target_os`.
    pub const fn for_platform() -> Self {
        if cfg!(target_os = "macos") {
            Self::Seatbelt
        } else if cfg!(target_os = "linux") {
            Self::Seccomp
        } else if cfg!(target_os = "freebsd") {
            Self::Capsicum
        } else {
            Self::None
        }
    }
}

// ── Shared helpers used by all platform modules ─────────────────────

/// Read `$SHELL` and extract the shell name.
pub(crate) fn detect_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| s.rsplit('/').next().map(String::from))
        .unwrap_or_else(|| "unknown".into())
}

/// Detect display server from env vars.
pub(crate) fn detect_display_server() -> String {
    if cfg!(target_os = "macos") {
        return "aqua".into();
    }
    if env_var_is_set("WAYLAND_DISPLAY") {
        "wayland".into()
    } else if env_var_is_set("DISPLAY") {
        "x11".into()
    } else {
        normalize_display_server(std::env::var("XDG_SESSION_TYPE").ok()).into()
    }
}

/// Read `$LANG`.
pub(crate) fn detect_locale() -> String {
    std::env::var("LANG").unwrap_or_else(|_| "unknown".into())
}

/// Detect IANA timezone.
pub(crate) fn detect_timezone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "unknown".into())
}

/// Hostname via libc `uname`.
pub(crate) fn detect_hostname() -> String {
    let mut buf: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut buf) } == 0 {
        let c_str = unsafe { std::ffi::CStr::from_ptr(buf.nodename.as_ptr()) };
        c_str.to_string_lossy().into_owned()
    } else {
        "unknown".into()
    }
}

/// OS version from libc `uname` release field.
pub(crate) fn detect_os_version() -> String {
    let mut buf: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut buf) } == 0 {
        let c_str = unsafe { std::ffi::CStr::from_ptr(buf.release.as_ptr()) };
        c_str.to_string_lossy().into_owned()
    } else {
        "unknown".into()
    }
}

/// Disk stats via `statvfs` on the current working directory.
pub(crate) fn detect_disk() -> (u64, u64) {
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c".".as_ptr(), &mut stat) } == 0 {
        let total = stat.f_blocks as u64 * stat.f_frsize as u64;
        let avail = stat.f_bavail as u64 * stat.f_frsize as u64;
        (total, avail)
    } else {
        (0, 0)
    }
}

/// Check if a non-loopback network interface appears to be up.
pub(crate) fn detect_has_network() -> bool {
    let mut addrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut addrs) } != 0 {
        return false;
    }

    let mut current = addrs;
    let mut has_network = false;

    while !current.is_null() {
        let ifaddr = unsafe { &*current };
        let family = if ifaddr.ifa_addr.is_null() {
            None
        } else {
            Some(unsafe { (*ifaddr.ifa_addr).sa_family as i32 })
        };

        if interface_is_network_up(ifaddr.ifa_flags as libc::c_uint, family) {
            has_network = true;
            break;
        }

        current = ifaddr.ifa_next;
    }

    unsafe { libc::freeifaddrs(addrs) };
    has_network
}

/// Detect terminal multiplexer from environment variables.
///
/// For tmux, `$TMUX` and `$TMUX_PANE` are always set.  Session name and
/// window/pane indices come from `tmux display-message` (fast, local IPC).
/// For zellij, `$ZELLIJ_SESSION_NAME` and `$ZELLIJ_PANE_ID` are set.
/// For screen, `$STY` carries the session pid.name.
pub(crate) fn detect_multiplexer() -> Option<MultiplexerInfo> {
    if env_var_is_set("TMUX") {
        return Some(detect_tmux());
    }
    if env_var_is_set("ZELLIJ") {
        return Some(detect_zellij());
    }
    if env_var_is_set("STY") {
        return Some(detect_screen());
    }
    None
}

fn detect_tmux() -> MultiplexerInfo {
    // Fast path: ask tmux for session:window.pane in one shot.
    let (session, window, pane) = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#S:#I.#P"])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // Parse "session_name:window_idx.pane_idx"
            let (sess, rest) = s.split_once(':')?;
            let (win, pan) = rest.split_once('.')?;
            Some((sess.to_string(), win.to_string(), pan.to_string()))
        })
        .unwrap_or_else(|| {
            // Fallback: env vars only (no window index available).
            let session = "unknown".into();
            let pane = std::env::var("TMUX_PANE")
                .unwrap_or_default()
                .trim_start_matches('%')
                .to_string();
            (session, String::new(), pane)
        });

    MultiplexerInfo { kind: "tmux".into(), session, window, pane }
}

fn detect_zellij() -> MultiplexerInfo {
    MultiplexerInfo {
        kind: "zellij".into(),
        session: std::env::var("ZELLIJ_SESSION_NAME").unwrap_or_default(),
        window: String::new(), // zellij doesn't expose tab index in env
        pane: std::env::var("ZELLIJ_PANE_ID").unwrap_or_default(),
    }
}

fn detect_screen() -> MultiplexerInfo {
    // $STY is "pid.name" (e.g. "12345.pts-0.hostname")
    let sty = std::env::var("STY").unwrap_or_default();
    let session = sty
        .split_once('.')
        .map(|(_, name)| name.to_string())
        .unwrap_or(sty);
    // $WINDOW is the screen window number
    let window = std::env::var("WINDOW").unwrap_or_default();

    MultiplexerInfo {
        kind: "screen".into(),
        session,
        window,
        pane: String::new(), // screen doesn't have sub-panes
    }
}

fn env_var_is_set(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

fn normalize_display_server(session_type: Option<String>) -> &'static str {
    match session_type.as_deref().map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("wayland") => "wayland",
        Some(value) if value.eq_ignore_ascii_case("x11") || value.eq_ignore_ascii_case("xorg") => {
            "x11"
        }
        _ => "none",
    }
}

fn interface_is_network_up(flags: libc::c_uint, family: Option<i32>) -> bool {
    let is_up = flags & (libc::IFF_UP as libc::c_uint) != 0;
    let is_loopback = flags & (libc::IFF_LOOPBACK as libc::c_uint) != 0;
    let has_ip_address = matches!(family, Some(libc::AF_INET) | Some(libc::AF_INET6));

    is_up && !is_loopback && has_ip_address
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "sysinfo_tests.rs"]
mod tests;
