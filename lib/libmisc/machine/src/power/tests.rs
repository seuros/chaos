use super::*;

#[test]
fn battery_power_is_about_current_supply_not_battery_charge_or_activity() {
    let mut batteries = vec![BatteryInfo {
        name: "BAT0".into(),
        kind: BatteryKind::System,
        present: Some(true),
        charge_percent: Some(0),
        state: BatteryState::Idle,
    }];
    let source = source_from_supplies(Some(true), Some(&batteries));
    let power = PowerInfo {
        source,
        external_power: Some(true),
        batteries: Some(batteries.clone()),
    };
    assert_eq!(power.on_battery_power(), Some(false));

    batteries[0].state = BatteryState::Discharging;
    assert_eq!(
        source_from_supplies(Some(true), Some(&batteries)),
        PowerSource::External
    );
    assert_eq!(
        source_from_supplies(Some(false), Some(&batteries)),
        PowerSource::Battery
    );
    batteries[0].kind = BatteryKind::Ups;
    assert_eq!(
        source_from_supplies(Some(false), Some(&batteries)),
        PowerSource::Ups
    );
    batteries[0].present = Some(false);
    assert_eq!(
        source_from_supplies(Some(true), Some(&batteries)),
        PowerSource::External
    );
    assert_eq!(
        source_from_supplies(Some(false), Some(&batteries)),
        PowerSource::Unknown
    );
    assert_eq!(PowerInfo::default().on_battery_power(), None);
}

#[test]
fn invalid_percentages_are_unknown_not_clamped() {
    for value in ["-1", "101", "256", "unknown", "NaN", ""] {
        assert_eq!(percent(value), None, "{value}");
    }
    assert_eq!(percent(" 0\n"), Some(0));
    assert_eq!(percent("100"), Some(100));
}

#[test]
fn charging_is_positive_evidence_even_without_an_adapter_sensor() {
    let batteries = [BatteryInfo {
        name: "BAT0".into(),
        kind: BatteryKind::System,
        present: Some(true),
        charge_percent: Some(4),
        state: BatteryState::Charging,
    }];
    assert_eq!(
        source_from_supplies(None, Some(&batteries)),
        PowerSource::External
    );
    assert_eq!(source_from_supplies(None, None), PowerSource::Unknown);
    assert_eq!(
        source_from_supplies(Some(false), Some(&[])),
        PowerSource::Unknown
    );
}
