use super::*;

fn from_values(values: &[(&str, i32)]) -> ThermalInfo {
    read_with(|name| {
        let name = name.to_str().ok()?;
        values
            .iter()
            .find_map(|(key, value)| (*key == name).then_some(*value))
    })
}

#[test]
fn temperature_and_tjmax_are_decikelvin_not_celsius() {
    let thermal = from_values(&[
        ("hw.ncpu", 1),
        ("dev.cpu.0.temperature", 3232),
        ("dev.cpu.0.coretemp.tjmax", 3732),
    ]);
    assert_eq!(thermal.state, crate::ThermalState::Unknown);
    assert_eq!(thermal.cpu_temperatures.len(), 1);
    let cpu = &thermal.cpu_temperatures[0];
    assert_eq!(cpu.id, "dev.cpu.0.temperature");
    assert_eq!(cpu.source, TemperatureSource::Sysctl);
    assert_eq!(cpu.kind, TemperatureKind::Unknown);
    assert!(
        cpu.celsius
            .is_some_and(|value| (value - 50.05).abs() < 0.000_001)
    );
    assert!(
        cpu.critical_celsius
            .is_some_and(|value| (value - 100.05).abs() < 0.000_001)
    );
}

#[test]
fn missing_cpu_sensors_do_not_hide_later_sensors() {
    let thermal = from_values(&[
        ("hw.ncpu", 4),
        ("dev.cpu.0.temperature", 0),
        ("dev.cpu.2.temperature", 3132),
        ("dev.cpu.2.coretemp.tjmax", -1),
    ]);
    assert_eq!(thermal.cpu_temperatures.len(), 2);
    assert_eq!(thermal.cpu_temperatures[0].celsius, None);
    assert_eq!(thermal.cpu_temperatures[1].id, "dev.cpu.2.temperature");
    assert!(thermal.cpu_temperatures[1].celsius.is_some());
    assert_eq!(thermal.cpu_temperatures[1].critical_celsius, None);
}

#[test]
fn absent_or_invalid_cpu_counts_do_not_trigger_sensor_queries() {
    for count in [None, Some(-1), Some(0), Some(4097), Some(i32::MAX)] {
        let thermal = read_with(|name| {
            assert_eq!(name, c"hw.ncpu");
            count
        });
        assert_eq!(thermal.state, crate::ThermalState::Unknown);
        assert!(thermal.cpu_temperatures.is_empty());
    }
}

#[test]
fn invalid_raw_temperatures_are_unknown() {
    for raw in [-1, 0, i32::MIN, i32::MAX] {
        assert_eq!(decikelvin(raw), None);
    }
}

fn os_state(zones: Option<&[&str]>, values: &[(&str, i32)]) -> ThermalState {
    let names = zones.map(|zones| {
        zones
            .iter()
            .map(|zone| CString::new(format!("hw.acpi.thermal.tz{zone}.thermal_flags")).unwrap())
            .collect::<Vec<_>>()
    });
    system_state(names.as_deref(), |name| {
        let name = name.to_str().ok()?;
        values
            .iter()
            .find_map(|(key, value)| (*key == name).then_some(*value))
    })
}

#[test]
fn acpi_warning_flags_not_measured_celsius_determine_os_severity() {
    let values = [
        ("hw.acpi.thermal.tz0.thermal_flags", 0),
        ("hw.acpi.thermal.tz0.temperature", 3132),
        ("hw.acpi.thermal.tz0._CRT", 3732),
    ];
    assert_eq!(os_state(Some(&["0"]), &values), ThermalState::Normal);
    assert_eq!(os_state(None, &values), ThermalState::Unknown);
    assert_eq!(os_state(Some(&[]), &values), ThermalState::Unknown);
    assert_eq!(os_state(Some(&["0", "2"]), &values), ThermalState::Unknown);
    for (flag, expected) in [
        (1, ThermalState::Warning),
        (4, ThermalState::Critical),
        (8, ThermalState::Critical),
        (16, ThermalState::Critical),
        (17, ThermalState::Critical),
        (2, ThermalState::Unknown),
        (-1, ThermalState::Unknown),
    ] {
        assert_eq!(
            os_state(Some(&["0"]), &[("hw.acpi.thermal.tz0.thermal_flags", flag)]),
            expected,
        );
    }
    assert_eq!(
        os_state(
            Some(&["0", "10"]),
            &[
                ("hw.acpi.thermal.tz0.thermal_flags", 1),
                ("hw.acpi.thermal.tz10.thermal_flags", 16),
            ]
        ),
        ThermalState::Critical,
    );
}

#[test]
fn zero_acpi_flags_without_valid_sensor_and_trip_support_are_not_normal() {
    for (temperature, trip) in [(Some(3132), None), (None, Some(3732)), (Some(0), Some(-1))] {
        let mut values = vec![("hw.acpi.thermal.tz0.thermal_flags", 0)];
        if let Some(temperature) = temperature {
            values.push(("hw.acpi.thermal.tz0.temperature", temperature));
        }
        if let Some(trip) = trip {
            values.push(("hw.acpi.thermal.tz0._CRT", trip));
        }
        assert_eq!(os_state(Some(&["0"]), &values), ThermalState::Unknown);
    }
    let unrelated = [CString::new("hw.acpi.thermal.polling_rate").unwrap()];
    assert_eq!(
        system_state(Some(&unrelated), |_| panic!("unrelated node")),
        ThermalState::Unknown
    );
}
