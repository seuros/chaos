use super::*;

#[test]
fn machine_warnings_defaults_and_partial_overrides() {
    let defaults: super::super::ConfigToml = toml::from_str("").unwrap();
    assert_eq!(defaults.machine_warnings, MachineWarningsConfig::default());
    let configured: super::super::ConfigToml =
        toml::from_str("[machine_warnings]\nbattery_percent = 8\ncpu_temperature_celsius = 90.0")
            .unwrap();
    assert_eq!(configured.machine_warnings.battery_percent, 8);
    assert_eq!(configured.machine_warnings.disk_free_percent, 5);
    assert_eq!(configured.machine_warnings.disk_free_bytes_cap, 20 << 30);
    assert_eq!(
        configured.machine_warnings.cpu_temperature_celsius,
        Some(90.0)
    );
    let home = tempfile::tempdir().unwrap();
    let loaded = super::super::Config::load_from_base_config_with_overrides(
        configured.clone(),
        Default::default(),
        home.path().to_path_buf(),
    )
    .unwrap();
    assert_eq!(loaded.machine_warnings, configured.machine_warnings);
}

#[test]
fn disk_byte_caps_are_positive_and_round_trip() {
    for cap in [1, 1 << 30, 20 << 30, u64::MAX] {
        let configured: MachineWarningsConfig =
            serde_json::from_value(serde_json::json!({"disk_free_bytes_cap": cap})).unwrap();
        assert_eq!(configured.disk_free_bytes_cap, cap);
        let restored: MachineWarningsConfig =
            serde_json::from_value(serde_json::to_value(&configured).unwrap()).unwrap();
        assert_eq!(restored, configured);
    }
}

#[test]
fn bounded_durations_preserve_endpoints_and_errors() {
    for (field, minimum, maximum, message) in [
        (
            "recovery_stable_seconds",
            30,
            86_400,
            "recovery duration must be in 30..=86400 seconds",
        ),
        (
            "recovery_instant_seconds",
            1,
            86_400,
            "recovery duration must be in 1..=86400 seconds",
        ),
        (
            "probe_timeout_ms",
            1,
            60_000,
            "probe timeout must be in 1..=60000 ms",
        ),
    ] {
        for value in [minimum, maximum] {
            let config: MachineWarningsConfig =
                serde_json::from_value(serde_json::json!({field: value})).unwrap();
            assert_eq!(serde_json::to_value(config).unwrap()[field], value);
        }
        for value in [minimum - 1, maximum + 1] {
            let error =
                serde_json::from_value::<MachineWarningsConfig>(serde_json::json!({field: value}))
                    .unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}

#[test]
fn machine_warnings_reject_invalid_thresholds_and_typos() {
    for setting in [
        "battery_percent = 101",
        "battery_percent = -1",
        "disk_free_percent = 256",
        "disk_free_percent = 5.5",
        "disk_free_bytes_cap = 0",
        "disk_free_bytes_cap = -1",
        "disk_free_bytes_cap = 1.5",
        "disk_free_bytes_cap = '20 GiB'",
        "cpu_temperature_celsius = nan",
        "cpu_temperature_celsius = inf",
        "cpu_temperature_celsius = 251.0",
        "cpu_temperature_celsius = -101.0",
        "enable = false",
        "probe_timeout_ms = 0",
        "probe_timeout_ms = 60001",
        "recovery_stable_seconds = 0",
        "recovery_stable_seconds = 86401",
        "recovery_instant_seconds = 0",
        "recovery_instant_seconds = 86401",
        "recovery_battery_margin_percent = 101",
        "recovery_battery_margin_percent = -1",
        "recovery_temperature_margin_celsius = nan",
        "recovery_temperature_margin_celsius = 0.0",
        "recovery_disk_margin_percent = 101",
        "recovery_disk_margin_bytes = -1",
    ] {
        assert!(
            toml::from_str::<MachineWarningsConfig>(setting).is_err(),
            "{setting}"
        );
    }
}
