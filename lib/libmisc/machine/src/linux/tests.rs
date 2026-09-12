use super::*;
use std::io;

fn supply(root: &Path, name: &str, fields: &[(&str, &str)]) -> io::Result<()> {
    let dir = root.join(name);
    fs::create_dir_all(&dir)?;
    for (key, value) in fields {
        fs::write(dir.join(key), value)?;
    }
    Ok(())
}

#[test]
fn removed_battery_on_ac_is_still_a_laptop_but_not_battery_powered() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let power_dir = root.path().join("sys/class/power_supply");
    supply(&power_dir, "AC", &[("type", "Mains"), ("online", "1")])?;
    supply(
        &power_dir,
        "BAT0",
        &[
            ("type", "Battery"),
            ("present", "0"),
            ("capacity", "0"),
            ("status", "Discharging"),
        ],
    )?;
    supply(root.path(), "sys/class/dmi/id", &[("chassis_type", "10")])?;
    let (profile, power) = detect_at(root.path());
    assert_eq!(profile.form_factor, crate::FormFactor::Laptop);
    assert_eq!(power.on_battery_power(), Some(false));
    assert_eq!(
        power
            .batteries
            .as_ref()
            .map(|batteries| batteries[0].present),
        Some(Some(false))
    );
    Ok(())
}

#[test]
fn dead_battery_on_ac_and_device_batteries_do_not_indicate_discharge() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    supply(root.path(), "AC", &[("type", "USB_PD"), ("online", "1")])?;
    supply(
        root.path(),
        "BAT0",
        &[
            ("type", "Battery"),
            ("present", "1"),
            ("capacity", "0"),
            ("status", "Not charging"),
        ],
    )?;
    supply(
        root.path(),
        "mouse",
        &[
            ("scope", "Device"),
            ("type", "Battery"),
            ("capacity", "1"),
            ("status", "Discharging"),
        ],
    )?;
    let power = read_power(root.path());
    assert_eq!(power.source, PowerSource::External);
    assert_eq!(power.batteries.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        power
            .batteries
            .as_ref()
            .map(|batteries| batteries[0].charge_percent),
        Some(Some(0))
    );
    Ok(())
}

#[test]
fn discharging_with_adapter_connected_and_multiple_batteries_preserves_facts() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    supply(root.path(), "AC", &[("type", "Mains"), ("online", "1")])?;
    supply(
        root.path(),
        "BAT0",
        &[
            ("type", "Battery"),
            ("capacity", "0"),
            ("status", "Not charging"),
        ],
    )?;
    supply(
        root.path(),
        "BAT1",
        &[
            ("type", "Battery"),
            ("capacity", "99"),
            ("status", "Discharging"),
        ],
    )?;
    let power = read_power(root.path());
    assert_eq!(power.source, PowerSource::External);
    assert_eq!(power.external_power, Some(true));
    assert_eq!(
        power.batteries.as_ref().map(|batteries| batteries
            .iter()
            .map(|battery| battery.charge_percent)
            .collect::<Vec<_>>()),
        Some(vec![Some(0), Some(99)])
    );
    Ok(())
}

#[test]
fn unavailable_incomplete_and_absent_supplies_are_distinct() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    assert_eq!(read_power(&root.path().join("missing")).batteries, None);
    assert_eq!(read_power(root.path()).batteries, Some(Vec::new()));
    supply(root.path(), "AC", &[("type", "Mains"), ("online", "1")])?;
    supply(root.path(), "broken", &[])?;
    let power = read_power(root.path());
    assert_eq!(power.batteries, None);
    assert_eq!(power.external_power, Some(true));
    assert_eq!(power.source, PowerSource::Unknown);
    Ok(())
}

#[test]
fn invalid_capacity_does_not_become_zero_and_unknown_state_is_not_discharge() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    supply(
        root.path(),
        "BAT0",
        &[("type", "Battery"), ("capacity", "101")],
    )?;
    let power = read_power(root.path());
    assert_eq!(
        power
            .batteries
            .as_ref()
            .map(|batteries| batteries[0].charge_percent),
        Some(None)
    );
    assert_eq!(power.on_battery_power(), None);
    Ok(())
}

#[test]
fn ups_is_a_separate_source_and_firmware_desktop_stays_desktop() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    supply(
        root.path(),
        "sys/class/power_supply/UPS0",
        &[
            ("type", "UPS"),
            ("capacity", "5"),
            ("status", "Discharging"),
        ],
    )?;
    supply(root.path(), "sys/class/dmi/id", &[("chassis_type", "3")])?;
    let (profile, power) = detect_at(root.path());
    assert_eq!(power.source, PowerSource::Ups);
    assert_eq!(profile.form_factor, crate::FormFactor::Desktop);
    Ok(())
}

#[test]
fn display_attachment_uses_connector_evidence_not_session_environment() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    assert_eq!(read_displays(root.path()), DisplayState::Unknown);
    supply(root.path(), "card0", &[])?;
    supply(root.path(), "card0-HDMI-A-1", &[("status", "disconnected")])?;
    assert_eq!(read_displays(root.path()), DisplayState::NoneDetected);
    supply(root.path(), "card0-eDP-1", &[("status", "unknown")])?;
    assert_eq!(read_displays(root.path()), DisplayState::Unknown);
    fs::write(root.path().join("card0-eDP-1/status"), "connected")?;
    assert_eq!(read_displays(root.path()), DisplayState::Connected);
    Ok(())
}

#[test]
fn containers_do_not_inherit_exposed_host_chassis_or_battery() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join(".dockerenv"), "")?;
    supply(root.path(), "sys/class/dmi/id", &[("chassis_type", "10")])?;
    supply(
        root.path(),
        "sys/class/power_supply/BAT0",
        &[
            ("type", "Battery"),
            ("capacity", "1"),
            ("status", "Discharging"),
        ],
    )?;
    let (profile, power) = detect_at(root.path());
    assert_eq!(
        profile.execution_environment,
        ExecutionEnvironment::Container
    );
    assert_eq!(profile.form_factor, crate::FormFactor::Unknown);
    assert_eq!(profile.displays, DisplayState::Unknown);
    assert_eq!(power, PowerInfo::default());
    Ok(())
}

#[test]
fn virtual_firmware_is_not_physical_chassis_evidence() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    supply(
        root.path(),
        "sys/class/dmi/id",
        &[
            ("chassis_type", "3"),
            ("product_name", "QEMU Virtual Machine"),
        ],
    )?;
    let (profile, _) = detect_at(root.path());
    assert_eq!(
        profile.execution_environment,
        ExecutionEnvironment::VirtualMachine
    );
    assert_eq!(profile.form_factor, crate::FormFactor::Unknown);
    Ok(())
}
