use super::*;
use crate::PowerSource;

#[test]
fn per_unit_capacity_and_state_are_not_collapsed_into_an_aggregate() {
    let batteries = collect(Some(3), |unit| match unit {
        0 => Some((12, 5)),  // critically discharging
        1 => Some((87, 2)),  // charging
        2 => Some((100, 7)), // removed, regardless of stale capacity
        _ => panic!("unexpected battery"),
    })
    .expect("complete inventory");
    assert_eq!(batteries.len(), 3);
    assert_eq!(batteries[0].name, "acpi0");
    assert_eq!(batteries[0].charge_percent, Some(12));
    assert_eq!(batteries[1].charge_percent, Some(87));
    assert_eq!(batteries[2].present, Some(false));
    assert_eq!(batteries[2].charge_percent, None);
    assert_eq!(batteries[2].state, BatteryState::Unknown);
    assert_eq!(
        from_supplies(Some(0), Some(batteries.clone())).source,
        PowerSource::Battery,
    );
    // AC assistance/maintenance discharge does not change the primary source.
    assert_eq!(
        from_supplies(Some(1), Some(batteries)).source,
        PowerSource::External
    );
}

#[test]
fn invalid_counts_partial_inventory_and_absent_supplies_stay_distinct() {
    for count in [None, Some(-1), Some(129), Some(i32::MAX)] {
        assert!(collect(count, |_| panic!("must not query invalid inventory")).is_none());
    }
    assert_eq!(
        collect(Some(0), |_| panic!("no batteries")),
        Some(Vec::new())
    );
    assert!(collect(Some(2), |unit| (unit == 0).then_some((50, 1))).is_none());
    assert_eq!(from_supplies(None, None), PowerInfo::default());
    assert_eq!(from_supplies(Some(1), None).source, PowerSource::External);
    assert_eq!(from_supplies(Some(0), None).source, PowerSource::Unknown);
}

#[test]
fn invalid_battery_data_is_not_clamped_or_treated_as_a_present_battery() {
    for flags in [-1, 3, 8, i32::MAX] {
        let battery = collect(Some(1), |_| Some((80, flags))).unwrap().remove(0);
        assert_eq!(battery.present, None);
        assert_eq!(battery.charge_percent, None);
        assert_eq!(battery.state, BatteryState::Unknown);
    }
    for capacity in [-1, 101, i32::MAX] {
        let battery = collect(Some(1), |_| Some((capacity, 1))).unwrap().remove(0);
        assert_eq!(battery.present, Some(true));
        assert_eq!(battery.charge_percent, None);
    }
    let dead = collect(Some(1), |_| Some((0, 0))).unwrap();
    assert_eq!(
        from_supplies(Some(1), Some(dead)).on_battery_power(),
        Some(false)
    );
}
