use super::*;
use crate::PowerSource;

#[test]
fn acpi_unknown_and_absent_are_not_clamped_to_a_full_battery() {
    assert_eq!(from_acpi(None, None, None, None), PowerInfo::default());
    let absent = from_acpi(Some(1), Some(0), Some(-1), Some(7));
    assert_eq!(absent.batteries, Some(Vec::new()));
    assert_eq!(absent.on_battery_power(), Some(false));

    let removed = from_acpi(Some(1), Some(1), Some(-1), Some(7));
    assert_eq!(
        removed
            .batteries
            .as_ref()
            .map(|batteries| batteries[0].present),
        Some(Some(false))
    );
    assert_eq!(
        removed
            .batteries
            .as_ref()
            .map(|batteries| batteries[0].charge_percent),
        Some(None)
    );
    assert_eq!(removed.source, PowerSource::External);
}

#[test]
fn dead_battery_on_ac_and_critical_discharge_are_distinct() {
    assert_eq!(
        from_acpi(Some(1), Some(1), Some(0), Some(0)).on_battery_power(),
        Some(false)
    );
    assert_eq!(
        from_acpi(Some(0), Some(1), Some(5), Some(5)).on_battery_power(),
        Some(true)
    );
    let invalid = from_acpi(Some(0), Some(1), Some(101), Some(1));
    assert_eq!(
        invalid
            .batteries
            .as_ref()
            .map(|batteries| batteries[0].charge_percent),
        Some(None)
    );
    assert_eq!(invalid.source, PowerSource::Battery);
}
