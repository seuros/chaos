//! Public-API tests for `chaos-uptime` — taming the arrow of time.
//!
//! Time only moves one way, but its rendering doesn't have to be a
//! mess. One dense table covers every branch and boundary of
//! `format_duration`: sub-second, sub-minute, minute+, and the exact
//! crossover points where entropy likes to hide off-by-one bugs.

use std::time::Duration;

use chaos_uptime::format_duration;

#[test]
fn format_duration_covers_all_branches_and_boundaries() {
    // (input_millis, expected_output, why_it_matters)
    let cases: &[(u64, &str, &str)] = &[
        // sub-second branch: bare millis, no decimals
        (0, "0ms", "zero must not panic or render as seconds"),
        (250, "250ms", "typical sub-second duration"),
        (999, "999ms", "upper edge of millis branch"),
        // sub-minute branch: 2-decimal seconds
        (1_000, "1.00s", "lower edge of seconds branch (1s exact)"),
        (1_500, "1.50s", "typical sub-minute duration"),
        (
            59_999,
            "60.00s",
            "59.999s rounds up to 60.00s, stays in seconds branch",
        ),
        // minute branch: "{m}m {ss:02}s"
        (
            60_000,
            "1m 00s",
            "exactly 1 minute crosses into minute branch",
        ),
        (75_000, "1m 15s", "typical multi-minute duration"),
        (
            3_600_000,
            "60m 00s",
            "1 hour renders as 60m, not 1h (no hour unit)",
        ),
        (
            3_601_000,
            "60m 01s",
            "second-precision survives at the hour mark",
        ),
    ];

    for (millis, expected, why) in cases {
        let got = format_duration(Duration::from_millis(*millis));
        assert_eq!(
            got, *expected,
            "format_duration({millis}ms) — {why}: expected {expected:?}, got {got:?}"
        );
    }
}
