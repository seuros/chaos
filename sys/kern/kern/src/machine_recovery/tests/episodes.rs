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

#[test]
fn a_checkpoint_from_before_a_context_reset_never_binds_to_a_later_episode() {
    let config = MachineWarningsConfig::default();
    let full = crate::config::test_config();
    let context = |path: &str| ObservationRequest::new(&full, std::path::Path::new(path));
    let mut recovery = Recovery::default();
    recovery.replace_context(context("/a"));
    sample(&mut recovery, &config, snapshot(&config, 90.0, 20 << 30), 0);
    let (stale, recovery_id) = recovery.checkpoint_recovery_id().unwrap();
    assert_eq!(recovery_id, None);

    // A -> B -> A: the context check alone cannot tell the episodes apart.
    recovery.replace_context(context("/b"));
    assert_eq!(recovery.phase(), Phase::Healthy);
    assert_eq!(recovery.episode(), None);
    recovery.replace_context(context("/a"));
    sample(&mut recovery, &config, snapshot(&config, 90.0, 20 << 30), 0);
    let (live, _) = recovery.checkpoint_recovery_id().unwrap();
    assert_ne!(live, stale, "a reset keeps issuing fresh episodes");

    recovery.bind_checkpoint_recovery(stale, 1);
    assert_eq!(recovery.status(&config, Duration::ZERO).recovery_id, None);
    recovery.bind_checkpoint_recovery(live, 2);
    assert_eq!(
        recovery.status(&config, Duration::ZERO).recovery_id,
        Some(2)
    );
}

#[test]
fn a_refused_observation_reads_as_unavailable_until_one_is_honoured() {
    let config = MachineWarningsConfig::default();
    let mut recovery = Recovery::default();
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 90.0, 20 << 30),
        100,
    );
    for seconds in (130..=430).step_by(30) {
        sample(
            &mut recovery,
            &config,
            snapshot(&config, 80.0, 20 << 30),
            seconds,
        );
    }
    assert_eq!(recovery.phase(), Phase::Recovered);

    // A renewed warning stamped before the latest observation is refused;
    // the stale Recovered must not stand for it.
    sample(
        &mut recovery,
        &config,
        snapshot(&config, 90.0, 20 << 30),
        50,
    );
    assert_eq!(recovery.phase(), Phase::Unavailable);
    assert!(recovery.unresolved());
    let status = recovery.status(&config, Duration::from_secs(430));
    assert_eq!(status.stable_seconds, 0);
    assert_eq!(
        status.blocked_reason.as_deref(),
        Some("recovery clock went backwards")
    );

    sample(
        &mut recovery,
        &config,
        snapshot(&config, 90.0, 20 << 30),
        460,
    );
    assert_eq!(recovery.phase(), Phase::Warning);
    assert_eq!(recovery.episode().map(Episode::raw), Some(2));
    assert_eq!(
        recovery.status(&config, Duration::ZERO).blocked_reason,
        None
    );
}
