use super::CreditsSnapshotDisplay;
use super::RateLimitSnapshotDisplay;
use super::RateLimitWindowDisplay;
use super::StatusRateLimitData;
use super::compose_rate_limit_data_many;
use jiff::Timestamp;
use pretty_assertions::assert_eq;

fn window(used_percent: f64) -> RateLimitWindowDisplay {
    RateLimitWindowDisplay {
        used_percent,
        resets_at: Some("soon".to_string()),
        resets_at_epoch_seconds: None,
        window_minutes: Some(300),
    }
}

pub(crate) fn status_rate_limits_suite() {
    non_codex_single_limit_renders_combined_row();
    non_codex_multi_limit_keeps_group_row();
}
#[cfg(test)]
fn non_codex_single_limit_renders_combined_row() {
    let now = Timestamp::now();
    let chaos = RateLimitSnapshotDisplay {
        limit_name: "chaos".to_string(),
        captured_at: now,
        primary: Some(window(10.0)),
        secondary: None,
        credits: Some(CreditsSnapshotDisplay {
            has_credits: true,
            unlimited: false,
            balance: Some("25".to_string()),
        }),
    };
    let other = RateLimitSnapshotDisplay {
        limit_name: "chaos-other".to_string(),
        captured_at: now,
        primary: Some(window(20.0)),
        secondary: None,
        credits: Some(CreditsSnapshotDisplay {
            has_credits: true,
            unlimited: false,
            balance: Some("99".to_string()),
        }),
    };

    let rows = match compose_rate_limit_data_many(&[chaos, other], now) {
        StatusRateLimitData::Available(rows) => rows,
        other => panic!("unexpected status: {other:?}"),
    };

    let labels: Vec<String> = rows.iter().map(|row| row.label.clone()).collect();
    assert_eq!(
        labels,
        vec![
            "5h limit".to_string(),
            "Credits".to_string(),
            "chaos-other 5h limit".to_string(),
            "Credits".to_string(),
        ]
    );
    assert_eq!(rows.iter().filter(|row| row.label == "Credits").count(), 2);
}

#[cfg(test)]
fn non_codex_multi_limit_keeps_group_row() {
    let now = Timestamp::now();
    let other = RateLimitSnapshotDisplay {
        limit_name: "chaos-other".to_string(),
        captured_at: now,
        primary: Some(RateLimitWindowDisplay {
            used_percent: 20.0,
            resets_at: Some("soon".to_string()),
            resets_at_epoch_seconds: None,
            window_minutes: Some(60),
        }),
        secondary: Some(RateLimitWindowDisplay {
            used_percent: 40.0,
            resets_at: Some("later".to_string()),
            resets_at_epoch_seconds: None,
            window_minutes: None,
        }),
        credits: None,
    };

    let rows = match compose_rate_limit_data_many(&[other], now) {
        StatusRateLimitData::Available(rows) => rows,
        other => panic!("unexpected status: {other:?}"),
    };
    let labels: Vec<String> = rows.iter().map(|row| row.label.clone()).collect();
    assert_eq!(
        labels,
        vec![
            "chaos-other limit".to_string(),
            "1h limit".to_string(),
            "Weekly limit".to_string(),
        ]
    );
}
