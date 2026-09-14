use super::*;
use pretty_assertions::assert_eq;
use rama::http::HeaderValue;

#[test]
fn parse_rate_limit_for_limit_defaults_to_codex_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-primary-used-percent",
        HeaderValue::from_static("12.5"),
    );
    headers.insert(
        "x-codex-primary-window-minutes",
        HeaderValue::from_static("60"),
    );
    headers.insert(
        "x-codex-primary-reset-at",
        HeaderValue::from_static("1704069000"),
    );
    headers.insert("x-codex-plan-type", HeaderValue::from_static("pro"));

    let snapshot = parse_rate_limit_for_limit_with_options(&headers, None, true).expect("snapshot");
    assert_eq!(snapshot.limit_id.as_deref(), Some("chaos"));
    assert_eq!(snapshot.limit_name, None);
    assert_eq!(snapshot.plan_type, Some(PlanType::Pro));
    let primary = snapshot.primary.expect("primary");
    assert_eq!(primary.used_percent, 12.5);
    assert_eq!(primary.window_minutes, Some(60));
    assert_eq!(primary.resets_at, Some(1704069000));
}

#[test]
fn codex_headers_are_ignored_unless_openai_codex_headers_are_enabled() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-primary-used-percent",
        HeaderValue::from_static("12.5"),
    );
    headers.insert(
        "x-codex-primary-window-minutes",
        HeaderValue::from_static("60"),
    );

    let snapshot = parse_rate_limit_for_limit(&headers, None).expect("snapshot");
    assert_eq!(snapshot.limit_id.as_deref(), Some("chaos"));
    assert_eq!(snapshot.primary, None);
    assert_eq!(snapshot.secondary, None);
    assert_eq!(snapshot.credits, None);
}

#[test]
fn parse_rate_limit_for_limit_falls_back_to_legacy_chaos_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-chaos-primary-used-percent",
        HeaderValue::from_static("12.5"),
    );
    headers.insert(
        "x-chaos-primary-window-minutes",
        HeaderValue::from_static("60"),
    );

    let snapshot = parse_rate_limit_for_limit_with_options(&headers, None, true).expect("snapshot");
    assert_eq!(snapshot.limit_id.as_deref(), Some("chaos"));
    let primary = snapshot.primary.expect("primary");
    assert_eq!(primary.used_percent, 12.5);
    assert_eq!(primary.window_minutes, Some(60));
}

#[test]
fn parse_rate_limit_for_explicit_default_limit_reads_codex_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-primary-used-percent",
        HeaderValue::from_static("12.5"),
    );

    let snapshot =
        parse_rate_limit_for_limit_with_options(&headers, Some("chaos"), true).expect("snapshot");
    assert_eq!(snapshot.limit_id.as_deref(), Some("chaos"));
    assert_eq!(
        snapshot.primary.as_ref().map(|window| window.used_percent),
        Some(12.5)
    );
}

#[test]
fn parse_rate_limit_for_limit_reads_secondary_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-chaos-secondary-primary-used-percent",
        HeaderValue::from_static("80"),
    );
    headers.insert(
        "x-chaos-secondary-primary-window-minutes",
        HeaderValue::from_static("1440"),
    );
    headers.insert(
        "x-chaos-secondary-primary-reset-at",
        HeaderValue::from_static("1704074400"),
    );

    let snapshot = parse_rate_limit_for_limit_with_options(&headers, Some("chaos_secondary"), true)
        .expect("snapshot");
    assert_eq!(snapshot.limit_id.as_deref(), Some("chaos_secondary"));
    assert_eq!(snapshot.limit_name, None);
    let primary = snapshot.primary.expect("primary");
    assert_eq!(primary.used_percent, 80.0);
    assert_eq!(primary.window_minutes, Some(1440));
    assert_eq!(primary.resets_at, Some(1704074400));
    assert_eq!(snapshot.secondary, None);
}

#[test]
fn parse_rate_limit_for_limit_prefers_limit_name_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-bengalfox-primary-used-percent",
        HeaderValue::from_static("80"),
    );
    headers.insert(
        "x-codex-bengalfox-limit-name",
        HeaderValue::from_static("gpt-5.4-codex-sonic"),
    );

    let snapshot = parse_rate_limit_for_limit_with_options(&headers, Some("chaos_bengalfox"), true)
        .expect("snapshot");
    assert_eq!(snapshot.limit_id.as_deref(), Some("chaos_bengalfox"));
    assert_eq!(snapshot.limit_name.as_deref(), Some("gpt-5.4-codex-sonic"));
}

#[test]
fn parse_all_rate_limits_reads_all_limit_families() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-primary-used-percent",
        HeaderValue::from_static("12.5"),
    );
    headers.insert(
        "x-codex-bengalfox-primary-used-percent",
        HeaderValue::from_static("80"),
    );

    let updates = parse_all_rate_limits(&headers, true);
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].limit_id.as_deref(), Some("chaos"));
    assert_eq!(updates[1].limit_id.as_deref(), Some("chaos_bengalfox"));
    assert_eq!(updates[0].limit_name, None);
    assert_eq!(updates[1].limit_name, None);
}

#[test]
fn parse_all_rate_limits_includes_default_codex_snapshot() {
    let headers = HeaderMap::new();

    let updates = parse_all_rate_limits(&headers, true);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].limit_id.as_deref(), Some("chaos"));
    assert_eq!(updates[0].limit_name, None);
    assert_eq!(updates[0].primary, None);
    assert_eq!(updates[0].secondary, None);
    assert_eq!(updates[0].credits, None);
}
