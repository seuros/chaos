use super::*;

#[test]
fn window_math_and_freshness() {
    // Raw counts derive utilization and surface the "85% left" answer.
    let w = UsageWindow::from_raw("tokens", 40_000, 34_000, Some(2_000), 1_000);
    assert_eq!(w.remaining_percent(), 85);
    assert_eq!(w.remaining_raw(), Some((34_000, 40_000)));
    assert!((w.remaining_fraction() - 0.85).abs() < 1e-9);

    // Freshness walks through the live → cached → reset progression
    // when a resets_at is known.
    assert_eq!(w.freshness(1_030), Freshness::Live);
    assert_eq!(w.freshness(1_500), Freshness::Cached);
    assert_eq!(w.freshness(2_000), Freshness::Reset);

    // Without a reset hint, aged observations eventually decay into
    // Stale rather than lingering as Cached forever.
    let unanchored = UsageWindow {
        label: "tokens".into(),
        limit: Some(40_000),
        remaining: Some(34_000),
        utilization: 0.15,
        resets_at: None,
        observed_at: 1_000,
    };
    assert_eq!(unanchored.freshness(1_030), Freshness::Live);
    assert_eq!(unanchored.freshness(1_120), Freshness::Cached);
    assert_eq!(unanchored.freshness(1_400), Freshness::Stale);

    // Percent-only windows (Claude MAX) bypass raw counts but still
    // answer the same question via utilization.
    let pct_only = UsageWindow {
        label: "5-hour".into(),
        limit: None,
        remaining: None,
        utilization: 0.15,
        resets_at: None,
        observed_at: 1_000,
    };
    assert_eq!(pct_only.remaining_percent(), 85);
    assert_eq!(pct_only.remaining_raw(), None);

    // limit=0 means "this budget does not exist" — render as
    // exhausted, not as 100% available.
    let zero = UsageWindow::from_raw("weird", 0, 0, None, 1_000);
    assert_eq!(zero.utilization, 1.0);
    assert_eq!(zero.remaining_percent(), 0);
}
