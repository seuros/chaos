use super::super::*;
use super::fixtures::*;

#[test]
fn disk_requires_both_margins_and_reports_impossible_targets() {
    let config = MachineWarningsConfig::default();
    let mut status = snapshot(&config, 70.0, 20 << 30);
    // On a small filesystem the absolute byte margin is the stronger requirement.
    status.storage.filesystems[0].total_bytes = 10 << 30;
    status.storage.filesystems[0].available_bytes = 1 << 30;
    let warning = snapshot(&config, 70.0, 1 << 30).warnings.remove(0);
    assert_eq!(verdict(&warning, &status, &config), Ok(false));
    status.storage.filesystems[0].available_bytes = 2 << 30;
    assert_eq!(verdict(&warning, &status, &config), Ok(true));
    status.storage.filesystems[0].total_bytes = 512 << 20;
    status.storage.filesystems[0].available_bytes = 400 << 20;
    assert_eq!(
        verdict(&warning, &status, &config),
        Err("recovery target exceeds filesystem capacity")
    );
}

#[test]
fn disk_reserves_past_f64_precision_compare_exactly_with_or_without_a_margin() {
    let total: u64 = 100 << 54;
    for margin_bytes in [0, 1] {
        let config = MachineWarningsConfig {
            disk_free_bytes_cap: u64::MAX,
            recovery_disk_margin_percent: 0,
            recovery_disk_margin_bytes: margin_bytes,
            ..Default::default()
        };
        let target = DiskThresholds::new(&config, total).recovery_bytes.unwrap();
        assert!(target > 1 << 53);
        let warning = snapshot_on_disk(&config, 70.0, total, 0).warnings.remove(0);
        for (available, recovered) in [(target - 1, false), (target, true)] {
            let status = snapshot_on_disk(&config, 70.0, total, available as u64);
            assert_eq!(verdict(&warning, &status, &config), Ok(recovered));
        }
    }
}

#[test]
fn large_disk_recovery_requires_capped_headroom_and_the_full_stable_window() {
    let config = MachineWarningsConfig::default();
    let mut recovery = Recovery::default();
    let status = |available| snapshot_on_disk(&config, 70.0, 4 << 40, available);
    sample(&mut recovery, &config, status(20 << 30), 0);
    assert_eq!(recovery.phase(), Phase::Warning);
    recovery.request_wait("turn").unwrap();

    for (seconds, available) in [(30, (20 << 30) + 1), (45, (21 << 30) - 1)] {
        let reading = status(available);
        assert!(reading.warnings.is_empty(), "raw warning cleared");
        sample(&mut recovery, &config, reading, seconds);
        assert_eq!(recovery.phase(), Phase::Warning, "headroom still missing");
    }
    // Disk headroom is instantaneous; only the short window applies.
    for seconds in (60..65).step_by(5) {
        sample(&mut recovery, &config, status(21 << 30), seconds);
        assert_eq!(recovery.phase(), Phase::Stabilizing);
    }
    sample(&mut recovery, &config, status(21 << 30), 65);
    assert_eq!(recovery.phase(), Phase::Recovered);
    assert_eq!(
        recovery
            .status(&config, Duration::from_secs(65))
            .stable_seconds,
        config.recovery_instant_seconds
    );
    sample(&mut recovery, &config, status(20 << 30), 95);
    assert_eq!(recovery.phase(), Phase::Warning);
}
