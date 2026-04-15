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
mod tests {
    use super::*;
    use crate::test_support::open_sqlite_store;
    use chaos_ration::UsageWindow;
    use rama_http_types::HeaderMap;
    use std::sync::Mutex;

    /// Hardcoded extractor: returns a single "tokens" window with a known
    /// observed_at regardless of headers, so the sniffer plumbing can be
    /// exercised without real provider traffic.
    struct FakeExtractor {
        calls: Arc<Mutex<usize>>,
    }

    impl HeaderExtractor for FakeExtractor {
        fn provider(&self) -> &str {
            "fake"
        }
        fn extract(&self, _headers: &HeaderMap, observed_at: i64) -> Vec<UsageWindow> {
            *self.calls.lock().unwrap() += 1;
            vec![UsageWindow::from_raw(
                "tokens",
                40_000,
                34_000,
                Some(observed_at + 3_600),
                observed_at,
            )]
        }
    }

    #[tokio::test]
    async fn sniffer_extracts_and_records_in_background() {
        let (_dir, store) = open_sqlite_store().await;

        let calls = Arc::new(Mutex::new(0usize));
        let extractor = FakeExtractor {
            calls: Arc::clone(&calls),
        };
        let sniffer = UsageSniffer::new(extractor, "https://example.com", Arc::clone(&store));
        sniffer.sniff(&HeaderMap::new());
        assert_eq!(*calls.lock().unwrap(), 1, "extractor ran exactly once");

        // Give the fire-and-forget writer a tick to commit.
        for _ in 0..50 {
            let rows = store.latest_all(0).await.expect("latest");
            if !rows.is_empty() {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].provider, "fake");
                assert_eq!(rows[0].base_url, "https://example.com");
                assert_eq!(rows[0].window.remaining_percent(), 85);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("background recording never landed");
    }
}
