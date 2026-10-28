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
        "uptime_secs",
        "sandbox_type",
        "in_container",
        "container_type",
        "shell",
        "display_server",
        "locale",
        "timezone",
        "has_network",
        "multiplexer",
    ];

    for key in expected_keys {
        assert!(obj.contains_key(key), "missing key: {key}");
    }
    // Dynamic observations have one owner: chaos-machine. Do not recreate a
    // second power/storage source in startup metadata or fabricate defaults.
    for key in [
        "disk_total",
        "disk_available",
        "has_battery",
        "battery_level",
        "charger_connected",
    ] {
        assert!(
            !obj.contains_key(key),
            "dynamic field in startup cache: {key}"
        );
    }
}

#[test]
fn display_server_types_are_normalized() {
    for (input, expected) in [
        (Some("wayland"), "wayland"),
        (Some("WAYLAND"), "wayland"),
        (Some("x11"), "x11"),
        (Some("Xorg"), "x11"),
        (Some("tty"), "none"),
        (Some("console"), "none"),
        (Some(""), "none"),
        (None, "none"),
    ] {
        assert_eq!(
            normalize_display_server(input.map(str::to_owned)),
            expected,
            "{input:?}"
        );
    }
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
