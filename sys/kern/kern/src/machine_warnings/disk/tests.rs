use super::*;

#[test]
fn defaults_cap_both_reserves_without_changing_small_disk_headroom() {
    let config = MachineWarningsConfig::default();
    let gib = 1_u128 << 30;
    for (total, warning, recovery) in [
        (20 << 30, gib, 2 * gib),
        (100 << 30, 5 * gib, 7 * gib),
        (4_000_000_000_000, 20 * gib, 21 * gib),
        (4 << 40, 20 * gib, 21 * gib),
        (u64::MAX, 20 * gib, 21 * gib),
    ] {
        assert_eq!(
            DiskThresholds::new(&config, total),
            DiskThresholds {
                warning_bytes: warning,
                recovery_bytes: Ok(recovery),
            },
            "total={total}"
        );
    }
}

#[test]
fn custom_reserves_keep_rounding_exact_and_apply_the_byte_margin_after_the_cap() {
    for (total, percent, cap, margin_percent, margin_bytes, warning, recovery) in [
        (101, 5, 100, 2, 1, 5, 8),
        (101, 5, 100, 0, 1, 5, 7),
        (101, 20, 15, 3, 1, 15, 16),
        (101, 0, 100, 2, 1, 0, 3),
        (101, 5, 1, 2, 1, 1, 2),
        (4 << 40, 5, 8 << 30, 2, 1 << 30, 8 << 30, 9 << 30),
        (
            u64::MAX,
            100,
            u64::MAX,
            0,
            0,
            u128::from(u64::MAX),
            u128::from(u64::MAX),
        ),
    ] {
        let config = MachineWarningsConfig {
            disk_free_percent: percent,
            disk_free_bytes_cap: cap,
            recovery_disk_margin_percent: margin_percent,
            recovery_disk_margin_bytes: margin_bytes,
            ..Default::default()
        };
        assert_eq!(
            DiskThresholds::new(&config, total),
            DiskThresholds {
                warning_bytes: warning,
                recovery_bytes: Ok(recovery),
            },
            "{config:?}, total={total}"
        );
    }
}

#[test]
fn impossible_recovery_does_not_suppress_the_warning_reserve_or_overflow() {
    for (total, percent, cap, margin_percent, margin_bytes, warning) in [
        (512 << 20, 5, 20 << 30, 2, 1 << 30, 26_843_545),
        (100, 5, 20, 96, 1, 5),
        (u64::MAX, 100, u64::MAX, 0, u64::MAX, u64::MAX),
    ] {
        let config = MachineWarningsConfig {
            disk_free_percent: percent,
            disk_free_bytes_cap: cap,
            recovery_disk_margin_percent: margin_percent,
            recovery_disk_margin_bytes: margin_bytes,
            ..Default::default()
        };
        assert_eq!(
            DiskThresholds::new(&config, total),
            DiskThresholds {
                warning_bytes: u128::from(warning),
                recovery_bytes: Err("recovery target exceeds filesystem capacity"),
            },
            "{config:?}, total={total}"
        );
    }
}
