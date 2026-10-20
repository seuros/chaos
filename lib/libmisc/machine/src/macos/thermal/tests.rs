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

#[test]
fn diagnostics_preserve_both_sources_when_they_disagree() {
    for (level, pressure) in [(100, 0), (0, 2)] {
        let thermal = from_raw(0, level, pressure);
        assert_eq!(thermal.state, ThermalState::Warning);
        assert_eq!(
            serde_json::to_value(&thermal).unwrap(),
            serde_json::json!({
                "state":"warning", "cpu_temperatures":[],
                "macos":{
                    "iopm_return_code":0,
                    "iopm_warning_level":level,
                    "ns_process_info_thermal_state":pressure,
                }
            })
        );
    }
}

#[test]
fn failed_iopm_query_does_not_publish_an_initialized_or_partial_level() {
    for level in [0, 100, 255] {
        let thermal = from_raw(-1, level, 0);
        assert_eq!(thermal.state, ThermalState::Unknown);
        let value = serde_json::to_value(&thermal).unwrap();
        assert_eq!(value["macos"]["iopm_return_code"], -1);
        assert_eq!(
            value["macos"]["iopm_warning_level"],
            serde_json::Value::Null
        );
        assert_eq!(value["macos"]["ns_process_info_thermal_state"], 0);
    }
    // A failed IOPM query must not discard a warning from the other source.
    assert_eq!(from_raw(-1, 255, 2).state, ThermalState::Warning);
}

#[test]
fn successful_unknown_values_are_preserved_without_inventing_a_state() {
    let thermal = from_raw(0, 777, -2);
    assert_eq!(thermal.state, ThermalState::Unknown);
    let raw = thermal.macos.unwrap();
    assert_eq!(raw.iopm_warning_level, Some(777));
    assert_eq!(raw.ns_process_info_thermal_state, -2);
}

#[test]
fn each_sample_reports_its_own_raw_values_without_latching_prior_warnings() {
    let hot = from_raw(0, 100, 2);
    let cool = from_raw(0, 0, 0);
    assert_eq!(hot.state, ThermalState::Warning);
    assert_eq!(cool.state, ThermalState::Normal);
    assert_eq!(cool.macos.unwrap().iopm_warning_level, Some(0));
    assert_eq!(hot.macos.unwrap().iopm_warning_level, Some(100));
}
