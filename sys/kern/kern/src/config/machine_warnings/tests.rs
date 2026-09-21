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
fn machine_warnings_reject_invalid_thresholds_and_typos() {
    for setting in [
        "battery_percent = 101",
        "battery_percent = -1",
        "disk_free_percent = 256",
        "disk_free_percent = 5.5",
        "cpu_temperature_celsius = nan",
        "cpu_temperature_celsius = inf",
        "cpu_temperature_celsius = 251.0",
        "cpu_temperature_celsius = -101.0",
        "enable = false",
        "probe_timeout_ms = 0",
        "probe_timeout_ms = 60001",
        "recovery_stable_seconds = 0",
        "recovery_stable_seconds = 86401",
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
