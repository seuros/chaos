use chaos_clamp::RateLimitInfo;
use chaos_clamp::UsageWindow;
use chaos_ipc::protocol::RateLimitSnapshot;
use chaos_ipc::protocol::RateLimitWindow;

const FIVE_HOURS_MINUTES: i64 = 5 * 60;
const SEVEN_DAYS_MINUTES: i64 = 7 * 24 * 60;

/// The account limits a clamped Claude Code reported, as Chaos shows them:
/// the five-hour window first, the seven-day window second.
pub(crate) fn snapshot(info: &RateLimitInfo) -> Option<RateLimitSnapshot> {
    let windows = info.unified_windows.as_ref();
    let reported = |kind: &str| {
        (info.rate_limit_type.as_deref() == Some(kind))
            .then(|| {
                // A rejected limit is exhausted even when no utilization came with it.
                let rejected = info.status.as_deref() == Some("rejected");
                info.utilization
                    .or_else(|| rejected.then_some(1.0))
                    .map(|utilization| UsageWindow {
                        utilization,
                        resets_at: info.resets_at.unwrap_or_default(),
                    })
            })
            .flatten()
    };
    let five_hour = windows
        .and_then(|windows| windows.five_hour.clone())
        .or_else(|| reported("five_hour"));
    let seven_day = windows
        .and_then(|windows| windows.seven_day.clone())
        .or_else(|| reported("seven_day"));
    if five_hour.is_none() && seven_day.is_none() {
        return None;
    }
    Some(RateLimitSnapshot {
        limit_id: Some("claude-code".to_owned()),
        limit_name: Some("Claude Code".to_owned()),
        primary: five_hour.map(|window| rate_limit_window(&window, FIVE_HOURS_MINUTES)),
        secondary: seven_day.map(|window| rate_limit_window(&window, SEVEN_DAYS_MINUTES)),
        credits: None,
        plan_type: None,
    })
}

fn rate_limit_window(window: &UsageWindow, minutes: i64) -> RateLimitWindow {
    RateLimitWindow {
        used_percent: (window.utilization * 100.0).clamp(0.0, 100.0),
        window_minutes: Some(minutes),
        resets_at: (window.resets_at > 0).then_some(window.resets_at),
    }
}

#[cfg(test)]
mod tests;
