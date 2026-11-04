use super::super::*;
use super::fixtures::*;
use chaos_machine::*;

#[test]
fn power_recovers_on_external_power_or_observed_battery_headroom() {
    let config = MachineWarningsConfig::default();
    let mut status = snapshot(&config, 70.0, 20 << 30);
    let warning = MachineWarning::LowBattery {
        name: "UPS".into(),
        battery_kind: BatteryKind::Ups,
        charge_percent: 1,
    };
    status.machine.power = PowerInfo::default();
    assert!(recovered(&warning, &status, &config).is_err());
    status.machine.power.source = PowerSource::Ups;
    status.machine.power.external_power = Some(false);
    assert!(recovered(&warning, &status, &config).is_err());
    status.machine.power.batteries = Some(vec![BatteryInfo {
        name: "UPS".into(),
        kind: BatteryKind::Ups,
        present: Some(true),
        charge_percent: Some(9),
        state: BatteryState::Discharging,
    }]);
    assert_eq!(recovered(&warning, &status, &config), Ok(false));
    for charge in [10, 100] {
        status.machine.power.batteries.as_mut().unwrap()[0].charge_percent = Some(charge);
        assert_eq!(recovered(&warning, &status, &config), Ok(true));
    }
    let zero_margin = MachineWarningsConfig {
        recovery_battery_margin_percent: 0,
        ..config
    };
    status.machine.power.batteries.as_mut().unwrap()[0].charge_percent =
        Some(config.battery_percent);
    assert_eq!(recovered(&warning, &status, &zero_margin), Ok(true));
    status.machine.power.batteries.as_mut().unwrap()[0].charge_percent = None;
    assert!(recovered(&warning, &status, &config).is_err());
    status.machine.power.batteries.as_mut().unwrap()[0].charge_percent = Some(100);
    status.machine.power.batteries.as_mut().unwrap()[0].name = "replacement".into();
    assert!(recovered(&warning, &status, &config).is_err());
    status.machine.power.batteries.as_mut().unwrap()[0].name = "UPS".into();
    status.machine.power.batteries.as_mut().unwrap()[0].present = Some(false);
    assert!(recovered(&warning, &status, &config).is_err());
    status.machine.power.source = PowerSource::External;
    status.machine.power.external_power = Some(true);
    assert_eq!(recovered(&warning, &status, &config), Ok(true));

    let start = Instant::now();
    let mut recovery = Recovery::default();
    status.machine.power = PowerInfo {
        source: PowerSource::Battery,
        external_power: Some(false),
        batteries: Some(vec![BatteryInfo {
            name: "InternalBattery-0".into(),
            kind: BatteryKind::System,
            present: Some(true),
            charge_percent: Some(5),
            state: BatteryState::Discharging,
        }]),
    };
    status.warnings = crate::machine_warnings::evaluate(&config, &status.machine, &status.storage);
    sample(&mut recovery, &config, status.clone(), start, 0);
    assert_eq!(recovery.phase(), Phase::Warning);
    status.machine.power.batteries.as_mut().unwrap()[0].charge_percent = Some(100);
    status.warnings = crate::machine_warnings::evaluate(&config, &status.machine, &status.storage);
    // Power flips almost instantly once the battery headroom is real.
    sample(&mut recovery, &config, status.clone(), start, 30);
    assert_eq!(recovery.phase(), Phase::Stabilizing);
    let recovered_after = 30 + config.recovery_instant_seconds;
    sample(
        &mut recovery,
        &config,
        status.clone(),
        start,
        recovered_after - 1,
    );
    assert_eq!(recovery.phase(), Phase::Stabilizing);
    sample(
        &mut recovery,
        &config,
        status.clone(),
        start,
        recovered_after,
    );
    assert_eq!(recovery.phase(), Phase::Recovered);
    assert!(!recovery.unresolved());
    status.machine.power.batteries.as_mut().unwrap()[0].charge_percent = Some(5);
    status.warnings = crate::machine_warnings::evaluate(&config, &status.machine, &status.storage);
    sample(&mut recovery, &config, status, start, recovered_after + 30);
    assert_eq!(recovery.phase(), Phase::Warning);
    assert_eq!(recovery.generation, 2);
}
