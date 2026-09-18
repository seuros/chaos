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
use rama_http_types::HeaderMap;

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
        extract_windows(
            headers,
            observed_at,
            &["requests", "tokens"],
            |label, slot| format!("x-ratelimit-{slot}-{label}"),
            |raw, base| parse_duration_secs(raw).map(|d| base + d),
        )
    }
}

/// Extractor for the `anthropic-ratelimit-*` family.
pub struct AnthropicHeaders;

impl HeaderExtractor for AnthropicHeaders {
    fn provider(&self) -> &str {
        "anthropic"
    }

    fn extract(&self, headers: &HeaderMap, observed_at: i64) -> Vec<UsageWindow> {
        extract_windows(
            headers,
            observed_at,
            &["requests", "tokens", "input-tokens", "output-tokens"],
            |label, slot| format!("anthropic-ratelimit-{label}-{slot}"),
            |raw, _base| parse_rfc3339_secs(raw),
        )
    }
}

/// Shared extraction loop used by every provider extractor.
///
/// * `labels`     — the window names to iterate ("requests", "tokens", …).
/// * `header_name` — maps `(label, slot)` to a concrete header name; `slot`
///   is one of `"limit"`, `"remaining"`, or `"reset"`. The closure lets each
///   provider arrange the three components in whatever order its API dictates.
/// * `parse_reset` — converts the raw reset string plus `observed_at` into an
///   absolute unix-second timestamp, or `None` when the header is absent or
///   unparseable. Delta-based providers add to `observed_at`; absolute
///   providers ignore it.
fn extract_windows(
    headers: &HeaderMap,
    observed_at: i64,
    labels: &[&str],
    header_name: impl Fn(&str, &str) -> String,
    parse_reset: impl Fn(&str, i64) -> Option<i64>,
) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    for &label in labels {
        let limit = get_u64(headers, &header_name(label, "limit"));
        let remaining = get_u64(headers, &header_name(label, "remaining"));
        let resets_at = get_str(headers, &header_name(label, "reset"))
            .and_then(|raw| parse_reset(raw, observed_at));
        if let (Some(limit), Some(remaining)) = (limit, remaining) {
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

/// Parse an RFC-3339 timestamp into unix seconds. Anthropic emits
/// `YYYY-MM-DDTHH:MM:SSZ`; jiff handles that and every reasonable variant
/// without us rolling a civil-from-days algorithm by hand.
fn parse_rfc3339_secs(raw: &str) -> Option<i64> {
    let ts: jiff::Timestamp = raw.trim().parse().ok()?;
    Some(ts.as_second())
}

#[cfg(test)]
mod tests;
