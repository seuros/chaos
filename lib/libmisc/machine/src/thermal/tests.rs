use super::*;

#[test]
fn unavailable_thermals_are_not_normal() {
    let thermal = ThermalInfo::default();
    assert_eq!(thermal.state, ThermalState::Unknown);
    assert!(thermal.cpu_temperatures.is_empty());
}

#[test]
fn containers_do_not_report_host_thermals() {
    let thermal = inspect(ExecutionEnvironment::Container);
    assert_eq!(thermal.state, ThermalState::Unknown);
    assert!(thermal.cpu_temperatures.is_empty());
}

#[test]
fn sanity_checks_preserve_zero_subzero_and_overheating() {
    for value in [-12.5, 0.0, 42.25, 140.0] {
        assert_eq!(valid_celsius(value), Some(value));
    }
    for value in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -273.15,
        32_767.0,
    ] {
        assert_eq!(valid_celsius(value), None);
    }
}

#[test]
fn native_machine_snapshot_includes_thermal_observations() {
    let thermal = crate::inspect_machine().thermal;
    eprintln!("native thermal observation: {thermal:?}");
    for sensor in thermal.cpu_temperatures {
        assert!(!sensor.id.is_empty());
        assert!(!sensor.label.is_empty());
        for value in [sensor.celsius, sensor.critical_celsius]
            .into_iter()
            .flatten()
        {
            assert_eq!(valid_celsius(value), Some(value));
        }
    }
}
