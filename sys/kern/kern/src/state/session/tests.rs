use super::*;
use crate::chaos::make_session_configuration_for_tests;
use crate::protocol::RateLimitWindow;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn set_rate_limits_defaults_limit_id_to_codex_when_missing() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_rate_limits(
        "test-provider",
        RateLimitSnapshot {
            limit_id: None,
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 12.0,
                window_minutes: Some(60),
                resets_at: Some(100),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        },
    );

    assert_eq!(
        state
            .latest_rate_limits
            .as_ref()
            .and_then(|v| v.limit_id.clone()),
        Some("chaos".to_string())
    );
}

#[tokio::test]
async fn set_rate_limits_defaults_to_codex_when_limit_id_missing_after_other_bucket() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_rate_limits(
        "test-provider",
        RateLimitSnapshot {
            limit_id: Some("chaos_other".to_string()),
            limit_name: Some("chaos_other".to_string()),
            primary: Some(RateLimitWindow {
                used_percent: 20.0,
                window_minutes: Some(60),
                resets_at: Some(200),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        },
    );
    state.set_rate_limits(
        "test-provider",
        RateLimitSnapshot {
            limit_id: None,
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 30.0,
                window_minutes: Some(60),
                resets_at: Some(300),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        },
    );

    assert_eq!(
        state
            .latest_rate_limits
            .as_ref()
            .and_then(|v| v.limit_id.clone()),
        Some("chaos".to_string())
    );
}

#[tokio::test]
async fn set_rate_limits_carries_credits_and_plan_type_from_chaos_to_chaos_other() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_rate_limits(
        "test-provider",
        RateLimitSnapshot {
            limit_id: Some("chaos".to_string()),
            limit_name: Some("chaos".to_string()),
            primary: Some(RateLimitWindow {
                used_percent: 10.0,
                window_minutes: Some(60),
                resets_at: Some(100),
            }),
            secondary: None,
            credits: Some(crate::protocol::CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance: Some("50".to_string()),
            }),
            plan_type: Some(chaos_ipc::account::PlanType::Plus),
        },
    );

    state.set_rate_limits(
        "test-provider",
        RateLimitSnapshot {
            limit_id: Some("chaos_other".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 30.0,
                window_minutes: Some(120),
                resets_at: Some(200),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        },
    );

    assert_eq!(
        state.latest_rate_limits,
        Some(RateLimitSnapshot {
            limit_id: Some("chaos_other".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 30.0,
                window_minutes: Some(120),
                resets_at: Some(200),
            }),
            secondary: None,
            credits: Some(crate::protocol::CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance: Some("50".to_string()),
            }),
            plan_type: Some(chaos_ipc::account::PlanType::Plus),
        })
    );
}

#[test]
fn a_partial_snapshot_keeps_its_own_windows_but_never_another_providers() {
    let window = |used_percent| RateLimitWindow {
        used_percent,
        window_minutes: Some(300),
        resets_at: Some(1),
    };
    let claude = |primary, secondary| RateLimitSnapshot {
        limit_id: Some("claude-code".to_owned()),
        limit_name: None,
        primary,
        secondary,
        credits: None,
        plan_type: None,
    };
    let both = claude(Some(window(10.0)), Some(window(40.0)));
    let five_hour_only = merge_rate_limit_fields(
        Some(&both),
        Some(&both),
        claude(Some(window(20.0)), None),
        0,
    );
    assert_eq!(five_hour_only.primary.map(|w| w.used_percent), Some(20.0));
    assert_eq!(five_hour_only.secondary.map(|w| w.used_percent), Some(40.0));

    let other = RateLimitSnapshot {
        limit_id: Some("chaos".to_owned()),
        secondary: Some(window(90.0)),
        ..claude(Some(window(70.0)), None)
    };
    let switched = merge_rate_limit_fields(Some(&other), None, claude(Some(window(5.0)), None), 0);
    assert_eq!(
        switched.secondary, None,
        "another provider's window never carries over"
    );
}

#[tokio::test]
async fn partial_windows_survive_another_providers_limits_in_between() {
    let mut state = SessionState::new(make_session_configuration_for_tests().await);
    let window = |used_percent| RateLimitWindow {
        used_percent,
        window_minutes: Some(300),
        resets_at: Some(i64::MAX),
    };
    let snapshot = |limit_id: &str, primary, secondary| RateLimitSnapshot {
        limit_id: Some(limit_id.to_owned()),
        limit_name: None,
        primary,
        secondary,
        credits: None,
        plan_type: None,
    };
    state.set_rate_limits(
        "test-provider",
        snapshot("claude-code", Some(window(10.0)), Some(window(40.0))),
    );
    state.set_rate_limits("test-provider", snapshot("chaos", Some(window(70.0)), None));
    state.set_rate_limits(
        "test-provider",
        snapshot("claude-code", Some(window(20.0)), None),
    );
    let latest = state.latest_rate_limits.expect("limits");
    assert_eq!(latest.primary.map(|w| w.used_percent), Some(20.0));
    assert_eq!(latest.secondary.map(|w| w.used_percent), Some(40.0));
}

#[test]
fn a_window_past_its_reset_is_not_carried_forward() {
    let window = |used_percent, resets_at| RateLimitWindow {
        used_percent,
        window_minutes: Some(10_080),
        resets_at: Some(resets_at),
    };
    let snapshot = |primary, secondary| RateLimitSnapshot {
        limit_id: Some("claude-code".to_owned()),
        limit_name: None,
        primary,
        secondary,
        credits: None,
        plan_type: None,
    };
    let full_week = snapshot(Some(window(10.0, 500)), Some(window(100.0, 1_000)));
    let before_reset = merge_rate_limit_fields(
        None,
        Some(&full_week),
        snapshot(Some(window(20.0, 600)), None),
        999,
    );
    assert_eq!(before_reset.secondary.map(|w| w.used_percent), Some(100.0));
    let after_reset = merge_rate_limit_fields(
        None,
        Some(&full_week),
        snapshot(Some(window(20.0, 600)), None),
        1_000,
    );
    assert_eq!(after_reset.secondary, None);
}

#[tokio::test]
async fn another_provider_never_shows_this_providers_windows() {
    let mut state = SessionState::new(make_session_configuration_for_tests().await);
    let window = RateLimitWindow {
        used_percent: 80.0,
        window_minutes: Some(300),
        resets_at: Some(i64::MAX),
    };
    let chaos = |primary| RateLimitSnapshot {
        limit_id: Some("chaos".to_owned()),
        limit_name: None,
        primary,
        secondary: None,
        credits: None,
        plan_type: None,
    };
    state.set_rate_limits("openai|", chaos(Some(window)));
    state.set_rate_limits("openrouter|https://openrouter.ai/api/v1", chaos(None));
    assert_eq!(state.latest_rate_limits.and_then(|s| s.primary), None);
}

#[tokio::test]
async fn another_provider_never_shows_this_providers_credits_or_plan() {
    let mut state = SessionState::new(make_session_configuration_for_tests().await);
    let chaos = |plan_type| RateLimitSnapshot {
        limit_id: Some("chaos".to_owned()),
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        plan_type,
    };
    state.set_rate_limits("openai|", chaos(Some(chaos_ipc::account::PlanType::Plus)));
    state.set_rate_limits("openrouter|https://openrouter.ai/api/v1", chaos(None));
    assert_eq!(state.latest_rate_limits.and_then(|s| s.plan_type), None);
}
