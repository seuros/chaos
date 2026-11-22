use super::*;
use chaos_clamp::UnifiedWindows;
use pretty_assertions::assert_eq;

fn window(utilization: f64, resets_at: i64) -> UsageWindow {
    UsageWindow {
        utilization,
        resets_at,
    }
}

#[test]
fn unified_windows_become_primary_and_secondary() {
    let info = RateLimitInfo {
        status: Some("allowed_warning".to_owned()),
        unified_windows: Some(UnifiedWindows {
            five_hour: Some(window(0.82, 1_795_300_000)),
            seven_day: Some(window(1.4, 1_795_800_000)),
        }),
        ..RateLimitInfo::default()
    };
    let snapshot = snapshot(&info).expect("snapshot");
    assert_eq!(snapshot.limit_id.as_deref(), Some("claude-code"));
    assert_eq!(
        snapshot.primary,
        Some(RateLimitWindow {
            used_percent: 82.0,
            window_minutes: Some(300),
            resets_at: Some(1_795_300_000),
        })
    );
    let secondary = snapshot.secondary.expect("seven day");
    assert_eq!(
        secondary.used_percent, 100.0,
        "an exceeded limit shows as full"
    );
    assert_eq!(secondary.window_minutes, Some(10_080));
}

#[test]
fn a_single_limit_report_fills_its_own_window() {
    let info = RateLimitInfo {
        rate_limit_type: Some("seven_day".to_owned()),
        utilization: Some(0.5),
        resets_at: Some(1_795_800_000),
        ..RateLimitInfo::default()
    };
    let snapshot = snapshot(&info).expect("snapshot");
    assert_eq!(snapshot.primary, None);
    assert_eq!(snapshot.secondary.map(|w| w.used_percent), Some(50.0));
}

#[test]
fn a_status_without_any_window_is_not_a_snapshot() {
    let info = RateLimitInfo {
        status: Some("rejected".to_owned()),
        rate_limit_type: Some("overage".to_owned()),
        ..RateLimitInfo::default()
    };
    assert_eq!(snapshot(&info), None);
}

#[test]
fn a_rejected_limit_without_utilization_shows_as_full() {
    let info = RateLimitInfo {
        status: Some("rejected".to_owned()),
        rate_limit_type: Some("five_hour".to_owned()),
        resets_at: Some(1_795_300_000),
        ..RateLimitInfo::default()
    };
    let primary = snapshot(&info)
        .and_then(|s| s.primary)
        .expect("five hour window");
    assert_eq!(primary.used_percent, 100.0);
    assert_eq!(primary.resets_at, Some(1_795_300_000));
}
