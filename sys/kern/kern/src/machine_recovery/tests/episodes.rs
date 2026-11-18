use super::super::*;
use super::fixtures::*;

#[test]
fn renewed_warnings_probe_failures_and_missing_readings_reset_stability() {
    let config = MachineWarningsConfig::default();
    for failure in 0..3 {
        let mut recovery = Recovery::default();
        sample(&mut recovery, &config, snapshot(&config, 90.0, 20 << 30), 0);
        for seconds in (30..300).step_by(30) {
            sample(
                &mut recovery,
                &config,
                snapshot(&config, 80.0, 20 << 30),
                seconds,
            );
        }
        let mut status = snapshot(&config, 80.0, 20 << 30);
        match failure {
            0 => sample(
                &mut recovery,
                &config,
                snapshot(&config, 90.0, 20 << 30),
                300,
            ),
            1 => recovery.observe(
                &Err("probe failed".into()),
                &config,
                Duration::from_secs(300),
            ),
            _ => {
                status.machine.thermal.cpu_temperatures.clear();
                sample(&mut recovery, &config, status, 300);
            }
        }
        assert_ne!(recovery.phase(), Phase::Recovered, "case {failure}");
        assert_eq!(
            recovery
                .status(&config, Duration::from_secs(900))
                .stable_seconds,
            0
        );
    }
}

#[test]
fn episodes_not_samples_are_counted_and_no_wait_is_implicit() {
    let config = MachineWarningsConfig::default();
    let mut recovery = Recovery::default();
    assert!(recovery.request_wait("turn").is_err());
    for episode in 0..3 {
        let offset = episode * 360;
        sample(
            &mut recovery,
            &config,
            snapshot(&config, 90.0, 20 << 30),
            offset,
        );
        for seconds in (30..=330).step_by(30) {
            sample(
                &mut recovery,
                &config,
                snapshot(&config, 80.0, 20 << 30),
                offset + seconds,
            );
        }
        assert_eq!(recovery.phase(), Phase::Recovered);
        assert!(recovery.wait_id().is_none());
        assert!(!recovery.parked());
        sample(
            &mut recovery,
            &config,
            snapshot(&config, 86.0, 20 << 30),
            offset + 345,
        );
        assert_eq!(
            recovery.phase(),
            Phase::Recovered,
            "inside the margin, under threshold"
        );
    }
    let status = recovery.status(&config, Duration::from_secs(1100));
    assert_eq!(status.interruptions_last_hour[1].count, 3);
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 90.0, 20 << 30),
        1100,
    );
    recovery.request_wait("new turn").unwrap();
    assert!(recovery.cancel_wait().is_some());
    assert!(recovery.wait_id().is_none());
}

#[test]
fn a_new_warning_joins_the_outstanding_set_and_restarts_the_timer() {
    let config = MachineWarningsConfig::default();
    let mut recovery = Recovery::default();
    sample(&mut recovery, &config, snapshot(&config, 90.0, 20 << 30), 0);
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 80.0, 20 << 30),
        30,
    );
    sample(&mut recovery, &config, snapshot(&config, 80.0, 1 << 30), 60);
    assert_eq!(recovery.phase(), Phase::Warning);
    assert_eq!(
        recovery.status(&config, Duration::ZERO).outstanding.len(),
        2
    );
    assert_eq!(recovery.status(&config, Duration::ZERO).stable_seconds, 0);
}

#[test]
fn operator_input_during_a_model_sample_prevents_a_late_wait_registration() {
    let config = MachineWarningsConfig::default();
    let mut recovery = Recovery::default();
    sample(&mut recovery, &config, snapshot(&config, 90.0, 20 << 30), 0);
    recovery.begin_sample();
    recovery.cancel_wait(); // New owner input, even before the tool was dispatched.
    assert!(recovery.request_wait("old response").is_err());
    recovery.begin_sample();
    assert!(recovery.request_wait("new response").is_ok());
}
