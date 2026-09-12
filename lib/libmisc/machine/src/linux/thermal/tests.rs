use super::*;

fn device(root: &Path, name: &str, fields: &[(&str, &str)]) -> io::Result<()> {
    let dir = root.join(name);
    fs::create_dir_all(&dir)?;
    for (key, value) in fields {
        fs::write(dir.join(key), value)?;
    }
    Ok(())
}

#[test]
fn only_identified_cpu_channels_are_reported() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    device(
        root.path(),
        "sys/class/hwmon/hwmon0",
        &[
            ("name", "coretemp\n"),
            ("temp1_input", "52500\n"),
            ("temp1_label", "Package id 0\n"),
            ("temp1_crit", "100000\n"),
            ("temp2_input", "48250"),
            ("temp2_label", "Core 0"),
        ],
    )?;
    for (index, driver) in [(1, "nvme"), (2, "amdgpu"), (3, "acpitz")] {
        device(
            root.path(),
            &format!("sys/class/hwmon/hwmon{index}"),
            &[("name", driver), ("temp1_input", "120000")],
        )?;
    }
    let thermal = read_at(root.path());
    assert_eq!(thermal.state, crate::ThermalState::Unknown);
    assert_eq!(thermal.cpu_temperatures.len(), 2);
    let package = &thermal.cpu_temperatures[0];
    assert!(package.id.ends_with("/hwmon0/temp1_input"));
    assert_eq!(package.label, "coretemp: Package id 0");
    assert_eq!(package.source, TemperatureSource::Hwmon);
    assert_eq!(package.kind, TemperatureKind::Physical);
    assert_eq!(package.celsius, Some(52.5));
    assert_eq!(package.critical_celsius, Some(100.0));
    let core = &thermal.cpu_temperatures[1];
    assert_eq!(core.celsius, Some(48.25));
    assert_eq!(core.critical_celsius, None);
    Ok(())
}

#[test]
fn amd_control_temperature_is_not_die_temperature_even_without_labels() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    device(
        root.path(),
        "sys/class/hwmon/hwmon0",
        &[
            ("name", "k10temp"),
            ("temp1_input", "95000"),
            ("temp2_input", "75000"),
            ("temp2_label", "Tdie"),
        ],
    )?;
    let thermal = read_at(root.path());
    assert_eq!(thermal.cpu_temperatures.len(), 2);
    assert_eq!(thermal.cpu_temperatures[0].kind, TemperatureKind::Control);
    assert_eq!(thermal.cpu_temperatures[0].celsius, Some(95.0));
    assert_eq!(thermal.cpu_temperatures[1].kind, TemperatureKind::Physical);
    assert_eq!(thermal.cpu_temperatures[1].label, "k10temp: Tdie");
    assert_eq!(thermal.cpu_temperatures[1].celsius, Some(75.0));
    Ok(())
}

#[test]
fn faulted_disabled_unreadable_and_malformed_channels_stay_unknown() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let name = "sys/class/hwmon/hwmon0";
    device(
        root.path(),
        name,
        &[
            ("name", "coretemp"),
            ("temp1_input", "90000"),
            ("temp1_fault", "1"),
            ("temp2_input", "90000"),
            ("temp2_enable", "0"),
            ("temp3_input", "NaN"),
            ("temp4_input", "90000"),
            ("temp4_fault", "invalid"),
            ("temp5_input", "90000"),
            ("temp6_input", "2147483647"),
            ("temp7_input", "0"),
            ("temp7_enable", "1"),
            ("temp7_fault", "0"),
            ("temp8_input", "-12500"),
            ("temp9_input", "135000"),
            ("temp9_crit", "bad"),
        ],
    )?;
    // A flag path that exists but cannot be read as text is not an absent flag.
    fs::create_dir(root.path().join(name).join("temp5_enable"))?;
    let thermal = read_at(root.path());
    let readings: Vec<_> = thermal
        .cpu_temperatures
        .iter()
        .map(|sensor| sensor.celsius)
        .collect();
    assert_eq!(
        readings,
        [
            None,
            None,
            None,
            None,
            None,
            None,
            Some(0.0),
            Some(-12.5),
            Some(135.0)
        ]
    );
    assert_eq!(thermal.cpu_temperatures[8].critical_celsius, None);
    Ok(())
}

#[test]
fn cpu_zones_use_only_their_own_critical_trips() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    device(
        root.path(),
        "sys/class/thermal/thermal_zone0",
        &[
            ("type", "cpu0-thermal"),
            ("temp", "48500"),
            ("mode", "disabled"),
            ("trip_point_0_type", "passive"),
            ("trip_point_0_temp", "80000"),
            ("trip_point_1_type", "critical"),
            ("trip_point_1_temp", "100000"),
            ("trip_point_2_type", "critical"),
            ("trip_point_2_temp", "95000"),
            ("trip_point_3_type", "critical"),
            ("trip_point_3_temp", "invalid"),
        ],
    )?;
    device(
        root.path(),
        "sys/class/thermal/thermal_zone1",
        &[("type", "cpu_thermal")],
    )?;
    device(
        root.path(),
        "sys/class/thermal/thermal_zone2",
        &[("type", "gpu-thermal"), ("temp", "130000")],
    )?;
    let thermal = read_at(root.path());
    assert_eq!(thermal.cpu_temperatures.len(), 2);
    let cpu = &thermal.cpu_temperatures[0];
    assert_eq!(cpu.source, TemperatureSource::ThermalZone);
    assert_eq!(cpu.label, "cpu0-thermal");
    assert_eq!(cpu.celsius, Some(48.5));
    assert_eq!(cpu.critical_celsius, Some(95.0));
    assert_eq!(thermal.cpu_temperatures[1].celsius, None);
    assert_eq!(thermal.cpu_temperatures[1].critical_celsius, None);
    Ok(())
}

#[test]
fn zone_names_are_conservative() {
    for name in [
        "x86_pkg_temp",
        "cpu",
        "cpu-thermal",
        "cpu0",
        "cpu12_thermal",
    ] {
        assert!(cpu_zone(name), "{name}");
    }
    for name in [
        "acpitz",
        "nvme",
        "gpu",
        "soc_thermal",
        "cpu-fan",
        "cpu+1",
        "cpu_other",
    ] {
        assert!(!cpu_zone(name), "{name}");
    }
    for name in ["temp_input", "temp+1_input", "temp1_input_extra"] {
        assert_eq!(index(name, "temp", "_input"), None);
    }
}

#[test]
fn probes_are_fresh_and_missing_interfaces_do_not_mean_zero() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let initial = read_at(root.path());
    assert_eq!(initial.state, crate::ThermalState::Unknown);
    assert!(initial.cpu_temperatures.is_empty());

    let device_path = "sys/class/hwmon/hwmon0";
    device(
        root.path(),
        device_path,
        &[("name", "coretemp"), ("temp1_input", "40000")],
    )?;
    assert_eq!(read_at(root.path()).cpu_temperatures[0].celsius, Some(40.0));
    let input = root.path().join(device_path).join("temp1_input");
    fs::write(&input, "80000")?;
    assert_eq!(read_at(root.path()).cpu_temperatures[0].celsius, Some(80.0));
    fs::remove_file(&input)?;
    assert!(read_at(root.path()).cpu_temperatures.is_empty());
    fs::create_dir(input)?;
    assert_eq!(read_at(root.path()).cpu_temperatures[0].celsius, None);
    Ok(())
}
