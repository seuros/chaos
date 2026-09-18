//! Explicit rate-limit sniffing for transport code.
//!
//! Adapters hold an `Option<Arc<UsageSniffer>>` and call `sniff()` after
//! each response. A sniffer pairs a provider-specific [`HeaderExtractor`]
//! with the [`UsageStore`] that receives parsed windows, pinned to a
//! `base_url` so snapshots for configs that share a provider tag but
//! point at different endpoints (multi-account, proxy, staging mirror)
//! don't collide in the store.
//!
//! Persistence runs through the store's bounded background writer, so
//! the HTTP hot path never blocks on the database — a full queue drops
//! the snapshot with a `tracing::warn` and carries on.

use crate::UsageStore;
use chaos_ration::HeaderExtractor;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Type-erased pairing of an extractor with a usage store, suitable for
/// stashing in an `Arc` and threading through transport code. Construct
/// one per (provider, base_url) at boot and pass it by reference into
/// the request path.
pub struct UsageSniffer {
    extractor: Box<dyn HeaderExtractor>,
    base_url: Arc<str>,
    store: Arc<UsageStore>,
}

impl std::fmt::Debug for UsageSniffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageSniffer")
            .field("provider", &self.extractor.provider())
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl UsageSniffer {
    pub fn new<E>(extractor: E, base_url: impl Into<Arc<str>>, store: Arc<UsageStore>) -> Self
    where
        E: HeaderExtractor + 'static,
    {
        Self {
            extractor: Box::new(extractor),
            base_url: base_url.into(),
            store,
        }
    }

    /// Extract windows from `headers` and persist them in the background.
    pub fn sniff(&self, headers: &rama_http_types::HeaderMap) {
        sniff_and_record(
            self.extractor.as_ref(),
            &self.base_url,
            &self.store,
            headers,
        );
    }

    /// The endpoint this sniffer was built for — useful for debug output
    /// and for composing log context upstream.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Record rate-limit headers inline. Extracts windows via `extractor`
/// and enqueues them on the store's background writer; nothing blocks
/// the caller.
pub fn sniff_and_record<E>(
    extractor: &E,
    base_url: &str,
    store: &Arc<UsageStore>,
    headers: &rama_http_types::HeaderMap,
) where
    E: HeaderExtractor + ?Sized,
{
    let observed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let windows = extractor.extract(headers, observed_at);
    if windows.is_empty() {
        return;
    }
    store.enqueue(extractor.provider(), base_url, windows);
}

#[cfg(test)]
mod tests;
