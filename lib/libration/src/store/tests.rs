use super::*;
use crate::test_support::open_sqlite_store;

#[tokio::test]
async fn record_upsert_freshness_and_base_url_isolation() {
    let (_dir, store) = open_sqlite_store().await;

    // First snapshot for xai under its canonical base_url: 40k/34k at
    // t=1000 — "85% left".
    let first = UsageWindow::from_raw("tokens", 40_000, 34_000, Some(2_000), 1_000);
    store
        .record("xai", "https://api.x.ai/v1", &[first])
        .await
        .expect("record first");

    // Latest reflects the snapshot; observed within 60s of now=1030 → Live.
    let live = store.latest_for("xai", 1_030).await.expect("latest");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].freshness, Freshness::Live);
    assert_eq!(live[0].window.remaining_percent(), 85);
    assert_eq!(live[0].base_url, "https://api.x.ai/v1");

    // Past the observation minute but before reset → Cached.
    let cached = store.latest_for("xai", 1_500).await.expect("latest");
    assert_eq!(cached[0].freshness, Freshness::Cached);

    // Past resets_at → Reset: budget should be considered recovered.
    let reset = store.latest_for("xai", 2_000).await.expect("latest");
    assert_eq!(reset[0].freshness, Freshness::Reset);

    // Second snapshot under the same (provider, base_url) upserts.
    let second = UsageWindow::from_raw("tokens", 40_000, 10_000, Some(4_000), 3_000);
    store
        .record("xai", "https://api.x.ai/v1", &[second])
        .await
        .expect("record second");

    let after = store.latest_for("xai", 3_005).await.expect("latest");
    assert_eq!(
        after.len(),
        1,
        "upsert keeps one row per (provider, base_url, label)"
    );
    assert_eq!(after[0].window.remaining_percent(), 25);
    assert_eq!(after[0].freshness, Freshness::Live);

    // A second config for the same provider under a different
    // base_url must coexist, not stomp.
    let alt = UsageWindow::from_raw("tokens", 40_000, 20_000, Some(5_000), 3_000);
    store
        .record("xai", "https://proxy.internal/xai", &[alt])
        .await
        .expect("record alt");

    let both = store.latest_for("xai", 3_005).await.expect("latest");
    assert_eq!(both.len(), 2, "distinct base_urls keep distinct rows");
    let percents: Vec<u8> = both.iter().map(|w| w.window.remaining_percent()).collect();
    assert!(percents.contains(&25));
    assert!(percents.contains(&50));
}
