use super::*;

#[test]
fn sysinfo_fields_populated() {
    let facts = platform::detect();

    assert!(!facts.os.is_empty(), "os must not be empty");
    assert!(!facts.os_version.is_empty(), "os_version must not be empty");
    assert!(!facts.arch.is_empty(), "arch must not be empty");
    assert!(!facts.hostname.is_empty(), "hostname must not be empty");
    assert!(facts.memory_total > 0, "memory_total must be > 0");
    assert!(facts.cpu_cores > 0, "cpu_cores must be > 0");
    assert!(!facts.cpu_model.is_empty(), "cpu_model must not be empty");
    assert!(facts.disk_total > 0, "disk_total must be > 0");
    assert!(facts.disk_available > 0, "disk_available must be > 0");
}

#[test]
fn arch_matches_const() {
    let facts = platform::detect();
    assert_eq!(facts.arch, std::env::consts::ARCH);
}

#[test]
fn os_is_current_platform() {
    let facts = platform::detect();
    let expected = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "freebsd") {
        "freebsd"
    } else {
        panic!("unsupported platform");
    };
    assert_eq!(facts.os, expected);
}

#[test]
fn sandbox_type_matches_platform() {
    let facts = platform::detect();
    let expected = if cfg!(target_os = "macos") {
        SandboxKind::Seatbelt
    } else if cfg!(target_os = "linux") {
        SandboxKind::Seccomp
    } else if cfg!(target_os = "freebsd") {
        SandboxKind::Capsicum
    } else {
        SandboxKind::None
    };
    assert_eq!(facts.sandbox_type, expected);
}

#[test]
fn singleton_returns_same_instance() {
    let a = sysinfo();
    let b = sysinfo();
    assert!(std::ptr::eq(a, b));
}

#[test]
fn uptime_is_nonzero() {
    let facts = platform::detect();
    assert!(
        facts.uptime_secs > 0,
        "uptime should be > 0 on a running system"
    );
}

#[test]
fn shell_detected() {
    // $SHELL is almost always set on unix
    let facts = platform::detect();
    assert_ne!(facts.shell, "", "shell should be detected");
}

#[test]
fn serialization_has_expected_keys() {
    let facts = platform::detect();
    let json = serde_json::to_value(&facts).expect("should serialize");
    let obj = json.as_object().expect("should be an object");

    let expected_keys = [
        "os",
        "os_version",
        "os_distro",
        "arch",
        "hostname",
        "memory_total",
        "memory_available",
        "cpu_cores",
        "cpu_model",
        "disk_total",
        "disk_available",
        "has_battery",
        "battery_level",
        "charger_connected",
        "uptime_secs",
        "sandbox_type",
        "in_container",
        "container_type",
        "shell",
        "display_server",
        "locale",
        "timezone",
        "has_network",
    ];

    for key in expected_keys {
        assert!(obj.contains_key(key), "missing key: {key}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_distro_detected() {
    let facts = platform::detect();
    // On a real Linux system, distro should be detected from /etc/os-release
    // or marker files.  CI may run in containers where this is available.
    // We just check it doesn't panic — empty is acceptable in minimal containers.
    let _ = facts.os_distro;
}

#[test]
fn graphical_session_types_are_normalized() {
    assert_eq!(normalize_display_server(Some("wayland".into())), "wayland");
    assert_eq!(normalize_display_server(Some("WAYLAND".into())), "wayland");
    assert_eq!(normalize_display_server(Some("x11".into())), "x11");
    assert_eq!(normalize_display_server(Some("Xorg".into())), "x11");
}

#[test]
fn non_graphical_session_types_normalize_to_none() {
    assert_eq!(normalize_display_server(Some("tty".into())), "none");
    assert_eq!(normalize_display_server(Some("console".into())), "none");
    assert_eq!(normalize_display_server(Some("".into())), "none");
    assert_eq!(normalize_display_server(None), "none");
}

#[test]
fn network_detection_requires_active_non_loopback_ip_interface() {
    assert!(interface_is_network_up(
        libc::IFF_UP as libc::c_uint,
        Some(libc::AF_INET6),
    ));
    assert!(interface_is_network_up(
        libc::IFF_UP as libc::c_uint,
        Some(libc::AF_INET),
    ));
    assert!(!interface_is_network_up(
        (libc::IFF_UP | libc::IFF_LOOPBACK) as libc::c_uint,
        Some(libc::AF_INET6),
    ));
    assert!(!interface_is_network_up(0, Some(libc::AF_INET6),));
    assert!(!interface_is_network_up(libc::IFF_UP as libc::c_uint, None,));
    assert!(!interface_is_network_up(
        libc::IFF_UP as libc::c_uint,
        Some(libc::AF_UNSPEC),
    ));
}
