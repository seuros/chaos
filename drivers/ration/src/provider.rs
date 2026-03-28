use crate::{RationError, Usage};

/// The trait vendors implement. Chaos doesn't know about providers —
/// providers know about Chaos.
#[allow(async_fn_in_trait)]
pub trait UsageProvider: Send + Sync {
    /// Provider identifier (e.g., "anthropic", "openai", "local-ollama").
    fn name(&self) -> &str;

    /// Fetch current usage. Implementors handle their own auth, endpoints, parsing.
    async fn fetch_usage(&self) -> Result<Usage, RationError>;

    /// Whether this provider is currently configured and reachable.
    /// Default: try fetch, return true if it works.
    async fn is_available(&self) -> bool {
        self.fetch_usage().await.is_ok()
    }
}
