//! Header extractors for common provider rate-limit shapes.
//!
//! Every provider ships the numbers differently. These implementations
//! cover the two shapes chaos touches today:
//!
//! * [`OpenAICompatibleHeaders`] — the `x-ratelimit-*` family used by
//!   OpenAI, xAI, Groq, and most OpenAI-shaped gateways. Exposes raw
//!   request and token counters with an RFC-3339 duration-ish reset
//!   ("1s", "500ms", "6m0s").
//!
//! * [`AnthropicHeaders`] — the `anthropic-ratelimit-*` family, which
//!   uses ISO-8601 absolute timestamps for resets and splits tokens
//!   into input/output streams.

use chaos_ration::HeaderExtractor;
use chaos_ration::UsageWindow;
use rama::http::HeaderMap;

/// Extractor for the OpenAI-style `x-ratelimit-*` family.
///
/// The provider label is passed in because the same header shape is
/// emitted by OpenAI, xAI, Groq, and friends — the store needs to know
/// which bucket the snapshot lands in.
pub struct OpenAICompatibleHeaders {
    provider: String,
}

impl OpenAICompatibleHeaders {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
        }
    }
}

impl HeaderExtractor for OpenAICompatibleHeaders {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn extract(&self, headers: &HeaderMap, observed_at: i64) -> Vec<UsageWindow> {
        let mut windows = Vec::new();
        for label in ["requests", "tokens"] {
            let limit = get_u64(headers, &format!("x-ratelimit-limit-{label}"));
            let remaining = get_u64(headers, &format!("x-ratelimit-remaining-{label}"));
            let reset_in = get_str(headers, &format!("x-ratelimit-reset-{label}"))
                .and_then(parse_duration_secs);

            if let (Some(limit), Some(remaining)) = (limit, remaining) {
                let resets_at = reset_in.map(|d| observed_at + d);
                windows.push(UsageWindow::from_raw(
                    label,
                    limit,
                    remaining,
                    resets_at,
                    observed_at,
                ));
            }
        }
        windows
    }
}

/// Extractor for the `anthropic-ratelimit-*` family.
pub struct AnthropicHeaders;

impl HeaderExtractor for AnthropicHeaders {
    fn provider(&self) -> &str {
        "anthropic"
    }

    fn extract(&self, headers: &HeaderMap, observed_at: i64) -> Vec<UsageWindow> {
        let mut windows = Vec::new();
        for label in ["requests", "tokens", "input-tokens", "output-tokens"] {
            let limit = get_u64(headers, &format!("anthropic-ratelimit-{label}-limit"));
            let remaining = get_u64(headers, &format!("anthropic-ratelimit-{label}-remaining"));
            let reset = get_str(headers, &format!("anthropic-ratelimit-{label}-reset"))
                .and_then(parse_rfc3339_secs);

            if let (Some(limit), Some(remaining)) = (limit, remaining) {
                windows.push(UsageWindow::from_raw(
                    label,
                    limit,
                    remaining,
                    reset,
                    observed_at,
                ));
            }
        }
        windows
    }
}

fn get_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn get_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    get_str(headers, name)?.parse::<u64>().ok()
}

/// Parse OpenAI's reset duration strings ("6m0s", "500ms", "1s") into
/// whole seconds. Milliseconds round up so a sub-second reset still
/// registers as imminent rather than "already reset".
fn parse_duration_secs(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let mut total_ms: i64 = 0;
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let mut j = i;
        while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
            j += 1;
        }
        if j == i {
            return None;
        }
        let num: f64 = std::str::from_utf8(&bytes[i..j]).ok()?.parse().ok()?;
        i = j;
        let mut k = i;
        while k < bytes.len() && bytes[k].is_ascii_alphabetic() {
            k += 1;
        }
        let unit = std::str::from_utf8(&bytes[i..k]).ok()?;
        i = k;
        let ms = match unit {
            "ms" => num,
            "s" => num * 1_000.0,
            "m" => num * 60_000.0,
            "h" => num * 3_600_000.0,
            _ => return None,
        };
        total_ms = total_ms.saturating_add(ms.ceil() as i64);
    }
    Some((total_ms + 999) / 1_000)
}

/// Parse an RFC-3339 timestamp into unix seconds. Pulls in nothing heavy
/// because this only handles the subset Anthropic emits: `YYYY-MM-DDTHH:MM:SSZ`.
fn parse_rfc3339_secs(raw: &str) -> Option<i64> {
    let raw = raw.trim().strip_suffix('Z')?;
    // YYYY-MM-DDTHH:MM:SS
    if raw.len() < 19 {
        return None;
    }
    let year: i64 = raw.get(0..4)?.parse().ok()?;
    let month: i64 = raw.get(5..7)?.parse().ok()?;
    let day: i64 = raw.get(8..10)?.parse().ok()?;
    let hour: i64 = raw.get(11..13)?.parse().ok()?;
    let minute: i64 = raw.get(14..16)?.parse().ok()?;
    let second: i64 = raw.get(17..19)?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Howard Hinnant's civil-from-days algorithm — unix epoch = 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use rama::http::HeaderValue;

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
}
