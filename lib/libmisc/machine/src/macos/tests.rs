use super::*;

#[test]
fn direct_power_with_no_dead_or_removed_battery_is_not_discharge() {
    let desktop = parse_power("Now drawing from 'AC Power'\n");
    assert_eq!(desktop.source, PowerSource::External);
    assert_eq!(desktop.external_power, Some(true));
    assert_eq!(desktop.batteries, Some(Vec::new()));
    for battery in [
        "-InternalBattery-0 (id=1)\t0%; not charging; present: true",
        "-InternalBattery-0 (id=1)\t0%; discharging; present: false",
    ] {
        let power = parse_power(&format!("Now drawing from 'AC Power'\n {battery}"));
        assert_eq!(power.on_battery_power(), Some(false), "{battery}");
    }
}

#[test]
fn battery_and_ups_supply_are_not_inferred_from_percentage_alone() {
    for (header, line, expected) in [
        (
            "Battery Power",
            "-InternalBattery-0 (id=1)\t5%; discharging;",
            PowerSource::Battery,
        ),
        (
            "AC Power",
            "-InternalBattery-0 (id=1)\t5%; charging;",
            PowerSource::External,
        ),
        (
            "AC Power",
            "-InternalBattery-0 (id=1)\t5%; discharging;",
            PowerSource::External,
        ),
        (
            "UPS Power",
            "-UPS0 (id=1)\t5%; discharging;",
            PowerSource::Ups,
        ),
    ] {
        let power = parse_power(&format!("Now drawing from '{header}'\n {line}"));
        assert_eq!(power.source, expected);
        assert_eq!(
            power
                .batteries
                .as_ref()
                .map(|batteries| batteries[0].charge_percent),
            Some(Some(5))
        );
    }
}

#[test]
fn unavailable_quartz_session_is_not_headless_evidence() {
    use crate::DisplayState;
    assert_eq!(display_state(0, 0, false), DisplayState::Unknown);
    assert_eq!(display_state(0, 0, true), DisplayState::NoneDetected);
    assert_eq!(display_state(0, 1, false), DisplayState::Connected);
    assert_eq!(display_state(1001, 1, true), DisplayState::Unknown);
}

#[test]
fn failed_or_malformed_power_output_is_not_an_absent_battery() {
    for text in ["", "permission denied", "Now drawing from 'Mystery Power'"] {
        assert_eq!(parse_power(text), PowerInfo::default());
    }
    for level in ["unknown", "101%", "-1%"] {
        let power = parse_power(&format!(
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t{level}; charging;"
        ));
        assert_eq!(
            power
                .batteries
                .as_ref()
                .map(|batteries| batteries[0].charge_percent),
            Some(None)
        );
    }
    let power = parse_power("Now drawing from 'AC Power'\n -UnrecognizedSupply\t0%;");
    assert_eq!(power.batteries, None);
}
