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
