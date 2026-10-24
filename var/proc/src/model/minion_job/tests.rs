use super::*;

#[test]
fn progress_counts_preserve_values_and_default_null_or_negative_counts() {
    let progress = MinionJobProgress::from(MinionJobProgressRow {
        total_items: 10,
        pending_items: Some(4),
        running_items: Some(3),
        completed_items: Some(2),
        failed_items: Some(1),
    });
    assert_eq!(
        progress,
        MinionJobProgress {
            total_items: 10,
            pending_items: 4,
            running_items: 3,
            completed_items: 2,
            failed_items: 1,
        }
    );
    let progress = MinionJobProgress::from(MinionJobProgressRow {
        total_items: -1,
        pending_items: None,
        running_items: Some(-2),
        completed_items: Some(0),
        failed_items: None,
    });
    assert_eq!(
        progress,
        MinionJobProgress {
            total_items: 0,
            pending_items: 0,
            running_items: 0,
            completed_items: 0,
            failed_items: 0,
        }
    );
}

#[test]
fn shared_epoch_conversion_accepts_negative_seconds_and_rejects_out_of_range_values() {
    for seconds in [-1, 0, 1_700_000_000] {
        assert_eq!(
            epoch_seconds_to_datetime(seconds)
                .expect("valid timestamp")
                .as_second(),
            seconds
        );
    }
    for seconds in [i64::MIN, i64::MAX] {
        let error = epoch_seconds_to_datetime(seconds).expect_err("timestamp out of range");
        assert!(
            error
                .to_string()
                .starts_with(&format!("invalid unix timestamp {seconds}:"))
        );
    }
}
