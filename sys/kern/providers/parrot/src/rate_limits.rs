use chaos_abi::RateLimitSnapshot;
use chaos_ipc::account::PlanType;
use chaos_ipc::protocol::CreditsSnapshot;
use chaos_ipc::protocol::RateLimitWindow;
use http::HeaderMap;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt::Display;

#[derive(Debug)]
pub struct RateLimitError {
    pub message: String,
}

impl Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Parses the default legacy Chaos rate-limit header family into a `RateLimitSnapshot`.
pub fn parse_default_rate_limit(headers: &HeaderMap) -> Option<RateLimitSnapshot> {
    parse_rate_limit_for_limit(headers, /*limit_id*/ None)
}

/// Parses all known rate-limit header families into update records keyed by limit id.
pub fn parse_all_rate_limits(
    headers: &HeaderMap,
    use_openai_codex_headers: bool,
) -> Vec<RateLimitSnapshot> {
    let mut snapshots = Vec::new();
    if let Some(snapshot) = parse_rate_limit_for_limit_with_options(
        headers,
        /*limit_id*/ None,
        use_openai_codex_headers,
    ) {
        snapshots.push(snapshot);
    }

    let mut limit_ids: BTreeSet<String> = BTreeSet::new();

    for name in headers.keys() {
        let header_name = name.as_str().to_ascii_lowercase();
        if let Some(limit_id) = header_name_to_limit_id(&header_name)
            .map(|limit_id| canonical_limit_id(limit_id, use_openai_codex_headers))
            && !matches!(limit_id.as_str(), "chaos")
        {
            limit_ids.insert(limit_id);
        }
    }

    snapshots.extend(limit_ids.into_iter().filter_map(|limit_id| {
        let snapshot = parse_rate_limit_for_limit_with_options(
            headers,
            Some(limit_id.as_str()),
            use_openai_codex_headers,
        )?;
        has_rate_limit_data(&snapshot).then_some(snapshot)
    }));

    snapshots
}

/// Parses rate-limit headers for the provided limit id.
///
/// `limit_id` should match the server-provided metered limit id (e.g. `chaos`,
/// `chaos_other`). When omitted, this defaults to the legacy `chaos` header family.
pub fn parse_rate_limit_for_limit(
    headers: &HeaderMap,
    limit_id: Option<&str>,
) -> Option<RateLimitSnapshot> {
    parse_rate_limit_for_limit_with_options(headers, limit_id, false)
}

/// Parses rate-limit headers with OpenAI Codex proxy aliases enabled.
///
/// `x-codex-*` is a ChatGPT/OpenAI-specific header family. Keep it opt-in so
/// OpenAI-compatible providers that only happen to use the Responses endpoint do
/// not have their unrelated `x-codex-*` headers interpreted as Chaos limits.
pub fn parse_rate_limit_for_limit_with_options(
    headers: &HeaderMap,
    limit_id: Option<&str>,
    use_openai_codex_headers: bool,
) -> Option<RateLimitSnapshot> {
    let requested_limit = limit_id.map(str::trim).filter(|name| !name.is_empty());
    let normalized_limit_id = requested_limit
        .map(normalize_limit_id)
        .map(|limit_id| canonical_limit_id(limit_id, use_openai_codex_headers))
        .unwrap_or_else(|| "chaos".to_string());
    let prefix = select_header_prefix(headers, requested_limit, use_openai_codex_headers);
    let primary = parse_rate_limit_window(
        headers,
        &format!("{prefix}-primary-used-percent"),
        &format!("{prefix}-primary-window-minutes"),
        &format!("{prefix}-primary-reset-at"),
    );

    let secondary = parse_rate_limit_window(
        headers,
        &format!("{prefix}-secondary-used-percent"),
        &format!("{prefix}-secondary-window-minutes"),
        &format!("{prefix}-secondary-reset-at"),
    );

    let credits = parse_credits_snapshot(headers, &prefix);
    let limit_name_header = format!("{prefix}-limit-name");
    let parsed_limit_name = parse_header_str(headers, &limit_name_header)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(std::string::ToString::to_string);

    Some(RateLimitSnapshot {
        limit_id: Some(normalized_limit_id),
        limit_name: parsed_limit_name,
        primary,
        secondary,
        credits,
        plan_type: parse_plan_type(headers, &prefix),
    })
}

#[derive(Debug, Deserialize)]
struct RateLimitEventWindow {
    used_percent: f64,
    window_minutes: Option<i64>,
    reset_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RateLimitEventDetails {
    primary: Option<RateLimitEventWindow>,
    secondary: Option<RateLimitEventWindow>,
}

#[derive(Debug, Deserialize)]
struct RateLimitEventCredits {
    has_credits: bool,
    unlimited: bool,
    balance: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RateLimitEvent {
    #[serde(rename = "type")]
    kind: String,
    plan_type: Option<PlanType>,
    rate_limits: Option<RateLimitEventDetails>,
    credits: Option<RateLimitEventCredits>,
    metered_limit_name: Option<String>,
    limit_name: Option<String>,
}

pub fn parse_rate_limit_event(payload: &str) -> Option<RateLimitSnapshot> {
    let event: RateLimitEvent = serde_json::from_str(payload).ok()?;
    if event.kind != "chaos.rate_limits" {
        return None;
    }
    let (primary, secondary) = if let Some(details) = event.rate_limits.as_ref() {
        (
            map_event_window(details.primary.as_ref()),
            map_event_window(details.secondary.as_ref()),
        )
    } else {
        (None, None)
    };
    let credits = event.credits.map(|credits| CreditsSnapshot {
        has_credits: credits.has_credits,
        unlimited: credits.unlimited,
        balance: credits.balance,
    });
    let limit_id = event
        .metered_limit_name
        .or(event.limit_name)
        .map(normalize_limit_id);
    Some(RateLimitSnapshot {
        limit_id: Some(limit_id.unwrap_or_else(|| "chaos".to_string())),
        limit_name: None,
        primary,
        secondary,
        credits,
        plan_type: event.plan_type,
    })
}

fn map_event_window(window: Option<&RateLimitEventWindow>) -> Option<RateLimitWindow> {
    let window = window?;
    Some(RateLimitWindow {
        used_percent: window.used_percent,
        window_minutes: window.window_minutes,
        resets_at: window.reset_at,
    })
}

/// Parses the bespoke Chaos rate-limit headers into a `RateLimitSnapshot`.
pub fn parse_promo_message(headers: &HeaderMap) -> Option<String> {
    parse_header_str(headers, "x-chaos-promo-message")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string)
}

fn parse_rate_limit_window(
    headers: &HeaderMap,
    used_percent_header: &str,
    window_minutes_header: &str,
    resets_at_header: &str,
) -> Option<RateLimitWindow> {
    let used_percent: Option<f64> = parse_header_f64(headers, used_percent_header);

    used_percent.and_then(|used_percent| {
        let window_minutes = parse_header_i64(headers, window_minutes_header);
        let resets_at = parse_header_i64(headers, resets_at_header);

        let has_data = used_percent != 0.0
            || window_minutes.is_some_and(|minutes| minutes != 0)
            || resets_at.is_some();

        has_data.then_some(RateLimitWindow {
            used_percent,
            window_minutes,
            resets_at,
        })
    })
}

fn parse_credits_snapshot(headers: &HeaderMap, prefix: &str) -> Option<CreditsSnapshot> {
    let has_credits = parse_header_bool(headers, &format!("{prefix}-credits-has-credits"))?;
    let unlimited = parse_header_bool(headers, &format!("{prefix}-credits-unlimited"))?;
    let balance = parse_header_str(headers, &format!("{prefix}-credits-balance"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string);
    Some(CreditsSnapshot {
        has_credits,
        unlimited,
        balance,
    })
}

fn parse_plan_type(headers: &HeaderMap, prefix: &str) -> Option<PlanType> {
    let raw = parse_header_str(headers, &format!("{prefix}-plan-type"))?
        .trim()
        .to_ascii_lowercase();
    serde_json::from_value(serde_json::Value::String(raw)).ok()
}

fn parse_header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    parse_header_str(headers, name)?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

fn parse_header_i64(headers: &HeaderMap, name: &str) -> Option<i64> {
    parse_header_str(headers, name)?.parse::<i64>().ok()
}

fn parse_header_bool(headers: &HeaderMap, name: &str) -> Option<bool> {
    let raw = parse_header_str(headers, name)?;
    if raw.eq_ignore_ascii_case("true") || raw == "1" {
        Some(true)
    } else if raw.eq_ignore_ascii_case("false") || raw == "0" {
        Some(false)
    } else {
        None
    }
}

fn parse_header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn has_rate_limit_data(snapshot: &RateLimitSnapshot) -> bool {
    snapshot.primary.is_some() || snapshot.secondary.is_some() || snapshot.credits.is_some()
}

fn header_name_to_limit_id(header_name: &str) -> Option<String> {
    let suffix = "-primary-used-percent";
    let prefix = header_name.strip_suffix(suffix)?;
    let limit = prefix.strip_prefix("x-")?;
    Some(normalize_limit_id(limit.to_string()))
}

fn normalize_limit_id(name: impl Into<String>) -> String {
    name.into().trim().to_ascii_lowercase().replace('-', "_")
}

fn canonical_limit_id(limit_id: String, use_openai_codex_headers: bool) -> String {
    if !use_openai_codex_headers {
        return limit_id;
    }

    if limit_id == "codex" {
        "chaos".to_string()
    } else if let Some(suffix) = limit_id.strip_prefix("codex_") {
        format!("chaos_{suffix}")
    } else {
        limit_id
    }
}

fn select_header_prefix(
    headers: &HeaderMap,
    requested_limit: Option<&str>,
    use_openai_codex_headers: bool,
) -> String {
    let candidates = header_prefix_candidates(requested_limit, use_openai_codex_headers);
    candidates
        .iter()
        .find(|prefix| header_prefix_has_data(headers, prefix))
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

fn header_prefix_candidates(
    requested_limit: Option<&str>,
    use_openai_codex_headers: bool,
) -> Vec<String> {
    let Some(requested_limit) = requested_limit else {
        if use_openai_codex_headers {
            return vec!["x-codex".to_string(), "x-chaos".to_string()];
        }
        return vec!["x-chaos".to_string()];
    };

    let normalized = normalize_limit_id(requested_limit);
    if matches!(normalized.as_str(), "chaos" | "codex") {
        if use_openai_codex_headers {
            return vec!["x-codex".to_string(), "x-chaos".to_string()];
        }
        return vec!["x-chaos".to_string()];
    }
    let hyphenated = normalized.replace('_', "-");
    let suffix = normalized
        .strip_prefix("chaos_")
        .or_else(|| {
            use_openai_codex_headers
                .then(|| normalized.strip_prefix("codex_"))
                .flatten()
        })
        .unwrap_or(normalized.as_str())
        .replace('_', "-");

    let mut candidates = if use_openai_codex_headers {
        vec![
            format!("x-codex-{suffix}"),
            format!("x-chaos-{suffix}"),
            format!("x-{hyphenated}"),
        ]
    } else {
        vec![format!("x-{hyphenated}")]
    };
    candidates.dedup();
    candidates
}

fn header_prefix_has_data(headers: &HeaderMap, prefix: &str) -> bool {
    [
        "primary-used-percent",
        "secondary-used-percent",
        "credits-has-credits",
        "plan-type",
        "limit-name",
    ]
    .iter()
    .any(|suffix| headers.contains_key(format!("{prefix}-{suffix}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;
    use pretty_assertions::assert_eq;

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

        let snapshot =
            parse_rate_limit_for_limit_with_options(&headers, None, true).expect("snapshot");
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

        let snapshot =
            parse_rate_limit_for_limit_with_options(&headers, None, true).expect("snapshot");
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

        let snapshot = parse_rate_limit_for_limit_with_options(&headers, Some("chaos"), true)
            .expect("snapshot");
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

        let snapshot =
            parse_rate_limit_for_limit_with_options(&headers, Some("chaos_secondary"), true)
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

        let snapshot =
            parse_rate_limit_for_limit_with_options(&headers, Some("chaos_bengalfox"), true)
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
}
