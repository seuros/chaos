use super::*;

#[test]
fn disk_requires_both_margins_and_reports_impossible_targets() {
    let config = MachineWarningsConfig::default();
    let mut status = snapshot(&config, 70.0, 20 << 30);
    // On a small filesystem the absolute byte margin is the stronger requirement.
    status.storage.filesystems[0].total_bytes = 10 << 30;
    status.storage.filesystems[0].available_bytes = 1 << 30;
    let warning = snapshot(&config, 70.0, 1 << 30).warnings.remove(0);
    assert_eq!(recovered(&warning, &status, &config), Ok(false));
    status.storage.filesystems[0].available_bytes = 2 << 30;
    assert_eq!(recovered(&warning, &status, &config), Ok(true));
    status.storage.filesystems[0].total_bytes = 512 << 20;
    status.storage.filesystems[0].available_bytes = 400 << 20;
    assert_eq!(
        recovered(&warning, &status, &config),
        Err("recovery target exceeds filesystem capacity")
    );
}

#[test]
fn large_disk_recovery_requires_capped_headroom_and_the_full_stable_window() {
    let config = MachineWarningsConfig::default();
    let start = Instant::now();
    let mut recovery = Recovery::default();
    let status = |available| snapshot_on_disk(&config, 70.0, 4 << 40, available);
    sample(&mut recovery, &config, status(20 << 30), start, 0);
    assert_eq!(recovery.phase(), Phase::Warning);
    recovery.request_wait("turn").unwrap();

    for (seconds, available) in [(30, (20 << 30) + 1), (45, (21 << 30) - 1)] {
        let reading = status(available);
        assert!(reading.warnings.is_empty(), "raw warning cleared");
        sample(&mut recovery, &config, reading, start, seconds);
        assert_eq!(recovery.phase(), Phase::Warning, "headroom still missing");
    }
    for seconds in (60..360).step_by(30) {
        sample(&mut recovery, &config, status(21 << 30), start, seconds);
        assert_eq!(recovery.phase(), Phase::Stabilizing);
    }
    sample(&mut recovery, &config, status(21 << 30), start, 360);
    assert_eq!(recovery.phase(), Phase::Recovered);
    assert_eq!(
        recovery
            .status(&config, start + Duration::from_secs(360))
            .stable_seconds,
        300
    );
    sample(&mut recovery, &config, status(20 << 30), start, 390);
    assert_eq!(recovery.phase(), Phase::Warning);
}
