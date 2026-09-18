use super::*;
use rama_http_types::HeaderValue;

fn hdr(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, v) in pairs {
        map.insert(*k, HeaderValue::from_str(v).unwrap());
    }
    map
}

#[test]
fn openai_compatible_parses_tokens_and_requests_with_reset_delta() {
    let h = hdr(&[
        ("x-ratelimit-limit-tokens", "40000"),
        ("x-ratelimit-remaining-tokens", "34000"),
        ("x-ratelimit-reset-tokens", "6m0s"),
        ("x-ratelimit-limit-requests", "500"),
        ("x-ratelimit-remaining-requests", "499"),
        ("x-ratelimit-reset-requests", "120ms"),
    ]);

    let ex = OpenAICompatibleHeaders::new("xai");
    let mut windows = ex.extract(&h, 1_000);
    windows.sort_by(|a, b| a.label.cmp(&b.label));

    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].label, "requests");
    assert_eq!(windows[0].remaining_raw(), Some((499, 500)));
    // 120ms rounds up to 1s → resets_at = observed + 1
    assert_eq!(windows[0].resets_at, Some(1_001));

    assert_eq!(windows[1].label, "tokens");
    assert_eq!(windows[1].remaining_percent(), 85);
    assert_eq!(windows[1].resets_at, Some(1_000 + 360));
}

#[test]
fn anthropic_parses_rfc3339_reset_and_all_four_windows() {
    let h = hdr(&[
        ("anthropic-ratelimit-requests-limit", "50"),
        ("anthropic-ratelimit-requests-remaining", "49"),
        ("anthropic-ratelimit-requests-reset", "2026-04-14T12:34:56Z"),
        ("anthropic-ratelimit-input-tokens-limit", "100000"),
        ("anthropic-ratelimit-input-tokens-remaining", "85000"),
        ("anthropic-ratelimit-output-tokens-limit", "20000"),
        ("anthropic-ratelimit-output-tokens-remaining", "19000"),
    ]);

    let ex = AnthropicHeaders;
    let mut windows = ex.extract(&h, 1_000);
    windows.sort_by(|a, b| a.label.cmp(&b.label));

    // requests + input-tokens + output-tokens (no bare `tokens` header here)
    assert_eq!(windows.len(), 3);
    let requests = windows.iter().find(|w| w.label == "requests").unwrap();
    assert_eq!(requests.resets_at, Some(1_776_170_096));
    let input = windows.iter().find(|w| w.label == "input-tokens").unwrap();
    assert_eq!(input.remaining_percent(), 85);
    let output = windows.iter().find(|w| w.label == "output-tokens").unwrap();
    assert_eq!(output.remaining_percent(), 95);
}

#[test]
fn empty_or_partial_headers_return_nothing() {
    let ex = OpenAICompatibleHeaders::new("xai");
    assert!(ex.extract(&HeaderMap::new(), 0).is_empty());
    // Limit without remaining is still dropped — the store needs both.
    let h = hdr(&[("x-ratelimit-limit-tokens", "40000")]);
    assert!(ex.extract(&h, 0).is_empty());
}
