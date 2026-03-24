use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Human label: "5-hour", "daily", "weekly", "monthly".
    pub label: String,

    /// How full the window is (0.0 = empty, 1.0 = exhausted).
    pub utilization: f64,

    /// When this window resets (unix timestamp), if known.
    pub resets_at: Option<i64>,
}
