use serde::Deserialize;
use serde::Serialize;

/// A usage snapshot from a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    /// Provider name (for display).
    pub provider: String,

    /// Active usage windows (short-term, weekly, monthly — whatever the provider exposes).
    pub windows: Vec<UsageWindow>,

    /// Remaining credits/balance, if the provider has a credit system.
    pub credits_remaining: Option<f64>,

    /// Total tokens consumed in current billing period.
    pub tokens_consumed: Option<u64>,

    /// When this snapshot was taken.
    pub fetched_at: i64,
}

/// A single rate-limit window.
///
/// Providers that expose raw counts (OpenAI, xAI, Anthropic rate-limit
/// headers) populate `limit` and `remaining`, and `utilization` is derived.
/// Providers that only expose percentages (Claude MAX session windows)
/// populate `utilization` directly and leave the raw counts as `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Human label: "tokens", "requests", "5-hour", "daily", "weekly".
    pub label: String,

    /// Raw cap if the provider exposes it (tokens, requests, etc.).
    pub limit: Option<u64>,

    /// Raw remaining count if the provider exposes it.
    pub remaining: Option<u64>,

    /// How full the window is (0.0 = empty, 1.0 = exhausted). Always set;
    /// derived from `remaining / limit` when raw counts are available.
    pub utilization: f64,

    /// When this window resets (unix seconds), if known.
    pub resets_at: Option<i64>,

    /// When this window was observed (unix seconds). Rate-limit headers
    /// only flow on requests, so stale observations are normal between
    /// bursts; consumers use this to reason about freshness.
    pub observed_at: i64,
}

/// How recent and usable a [`UsageWindow`] reading is.
///
/// Rate-limit headers only arrive on live responses, so between requests
/// the last-seen window goes stale. Past `resets_at`, the budget has
/// refilled and the old numbers are actively misleading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Freshness {
    /// Observed within the last minute — trust the numbers.
    Live,
    /// Older than a minute but the reset window has not yet passed.
    Cached,
    /// `resets_at` has elapsed — assume the budget recovered.
    Reset,
}

impl UsageWindow {
    /// Classify a reading against the current time.
    pub fn freshness(&self, now: i64) -> Freshness {
        if let Some(resets_at) = self.resets_at
            && resets_at <= now
        {
            return Freshness::Reset;
        }
        if now - self.observed_at < 60 {
            Freshness::Live
        } else {
            Freshness::Cached
        }
    }

    /// Fraction of budget still available (0.0 exhausted → 1.0 untouched).
    pub fn remaining_fraction(&self) -> f64 {
        (1.0 - self.utilization).clamp(0.0, 1.0)
    }

    /// Remaining budget as a whole percent, rounded. The "85% left" value.
    pub fn remaining_percent(&self) -> u8 {
        (self.remaining_fraction() * 100.0).round() as u8
    }

    /// `(remaining, limit)` when the provider exposes raw counts.
    pub fn remaining_raw(&self) -> Option<(u64, u64)> {
        self.limit.zip(self.remaining).map(|(l, r)| (r, l))
    }

    /// Build a window from raw counts, deriving `utilization` automatically.
    pub fn from_raw(
        label: impl Into<String>,
        limit: u64,
        remaining: u64,
        resets_at: Option<i64>,
        observed_at: i64,
    ) -> Self {
        let utilization = if limit == 0 {
            0.0
        } else {
            1.0 - (remaining as f64 / limit as f64)
        };
        Self {
            label: label.into(),
            limit: Some(limit),
            remaining: Some(remaining),
            utilization: utilization.clamp(0.0, 1.0),
            resets_at,
            observed_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_math_and_freshness() {
        // Raw counts derive utilization and surface the "85% left" answer.
        let w = UsageWindow::from_raw("tokens", 40_000, 34_000, Some(2_000), 1_000);
        assert_eq!(w.remaining_percent(), 85);
        assert_eq!(w.remaining_raw(), Some((34_000, 40_000)));
        assert!((w.remaining_fraction() - 0.85).abs() < 1e-9);

        // Freshness walks through the live → cached → reset progression.
        assert_eq!(w.freshness(1_030), Freshness::Live);
        assert_eq!(w.freshness(1_500), Freshness::Cached);
        assert_eq!(w.freshness(2_000), Freshness::Reset);

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

        // Zero-limit guard keeps derivation safe.
        let zero = UsageWindow::from_raw("weird", 0, 0, None, 1_000);
        assert_eq!(zero.utilization, 0.0);
    }
}
