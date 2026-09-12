use super::*;

#[test]
fn documented_warning_aliases_are_not_ordinal_severity() {
    assert_eq!(warning_state(0, 0), ThermalState::Normal);
    assert_eq!(warning_state(0, 100), ThermalState::Warning);
    assert_eq!(warning_state(0, 10), ThermalState::Critical);
}

#[test]
fn failed_or_unpublished_warning_level_is_not_normal() {
    for level in [0, 10, 100, 255] {
        assert_eq!(warning_state(-1, level), ThermalState::Unknown);
    }
    for level in [1, 5, 110, 255, u32::MAX] {
        assert_eq!(warning_state(0, level), ThermalState::Unknown);
    }
}

#[test]
fn pressure_can_establish_a_warning_but_nominal_cannot_establish_support() {
    for pressure in [0, -1, 4, isize::MAX] {
        assert_eq!(
            with_pressure(ThermalState::Unknown, pressure),
            ThermalState::Unknown
        );
    }
    for pressure in [1, 2] {
        assert_eq!(
            with_pressure(ThermalState::Unknown, pressure),
            ThermalState::Warning
        );
    }
    assert_eq!(
        with_pressure(ThermalState::Unknown, 3),
        ThermalState::Critical
    );
}

#[test]
fn pressure_never_downgrades_a_known_warning() {
    assert_eq!(with_pressure(ThermalState::Normal, 0), ThermalState::Normal);
    assert_eq!(
        with_pressure(ThermalState::Normal, 2),
        ThermalState::Warning
    );
    assert_eq!(
        with_pressure(ThermalState::Critical, 1),
        ThermalState::Critical
    );
    assert_eq!(
        with_pressure(ThermalState::Warning, 3),
        ThermalState::Critical
    );
}
