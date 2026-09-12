use super::*;
use crate::{BatteryInfo, BatteryState};

#[test]
fn chassis_is_independent_of_displays_and_battery_presence() {
    let power = PowerInfo::default();
    for (code, expected) in [
        (10, FormFactor::Laptop),
        (3, FormFactor::Desktop),
        (23, FormFactor::Server),
        (30, FormFactor::Tablet),
    ] {
        assert_eq!(
            classify(Some(code), None, &power),
            (expected, FormFactorEvidence::Firmware)
        );
    }
    let profile = MachineProfile {
        form_factor: classify(Some(10), None, &power).0,
        displays: DisplayState::NoneDetected,
        ..MachineProfile::default()
    };
    assert_eq!(profile.form_factor, FormFactor::Laptop);
    assert_eq!(profile.displays, DisplayState::NoneDetected);
    assert_eq!(classify(Some(2), None, &power).0, FormFactor::Unknown);
    assert_eq!(classify(None, None, &power).0, FormFactor::Unknown);
}

#[test]
fn model_names_are_evidence_but_generic_mac_identifiers_are_not() {
    let power = PowerInfo::default();
    assert_eq!(
        classify(None, Some("MacBookPro18,3"), &power).0,
        FormFactor::Laptop
    );
    assert_eq!(
        classify(None, Some("Macmini9,1"), &power).0,
        FormFactor::Desktop
    );
    assert_eq!(
        classify(None, Some("Mac14,2"), &power).0,
        FormFactor::Unknown
    );
}

#[test]
fn ups_is_not_laptop_evidence_and_missing_battery_is_not_desktop_evidence() {
    let mut power = PowerInfo {
        batteries: Some(vec![BatteryInfo {
            name: "UPS0".into(),
            kind: BatteryKind::Ups,
            present: Some(true),
            charge_percent: Some(100),
            state: BatteryState::Full,
        }]),
        ..PowerInfo::default()
    };
    assert_eq!(classify(None, None, &power).0, FormFactor::Unknown);
    if let Some(batteries) = &mut power.batteries {
        batteries[0].kind = BatteryKind::System;
    }
    assert_eq!(
        classify(None, None, &power),
        (FormFactor::Laptop, FormFactorEvidence::SystemBattery)
    );
    power.batteries = Some(Vec::new());
    assert_eq!(classify(None, None, &power).0, FormFactor::Unknown);
}
