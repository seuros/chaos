use crate::UsageWindow;
use rama::http::HeaderMap;

/// Pull rate-limit windows out of a provider's response headers.
///
/// Every provider with a rate-limit story ships the numbers as HTTP response
/// headers — the header names, count vs. percent semantics, and reset
/// encodings all differ. Each parrot implements this trait for the shape it
/// speaks; the middleware stays provider-agnostic and just calls [`extract`].
///
/// [`extract`]: HeaderExtractor::extract
pub trait HeaderExtractor: Send + Sync + 'static {
    /// Short provider tag persisted alongside the snapshot
    /// (e.g. `"openai"`, `"xai"`, `"anthropic"`, `"claude-max"`).
    fn provider(&self) -> &str;

    /// Parse every rate-limit window the provider advertises.
    ///
    /// `observed_at` is the unix-second timestamp the caller associates with
    /// the response; implementors stamp each returned [`UsageWindow`] with it
    /// so freshness can be judged later without threading clocks around.
    ///
    /// Returning an empty vec is normal — not every response carries every
    /// header, and some endpoints omit them entirely.
    fn extract(&self, headers: &HeaderMap, observed_at: i64) -> Vec<UsageWindow>;
}
