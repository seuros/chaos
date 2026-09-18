use super::*;

#[test]
fn discovery_workflow_covers_cache_and_fetch_paths() {
    let mut workflow = ModelDiscoveryWorkflow::new();
    workflow.begin(RefreshStrategy::Offline);
    assert_eq!(workflow.state(), ModelDiscoveryState::CheckingCache);

    let mut workflow = ModelDiscoveryWorkflow::new();
    workflow.begin(RefreshStrategy::OnlineIfUncached);
    workflow.record_cache_hit();
    assert_eq!(workflow.state(), ModelDiscoveryState::CachedCatalog);

    let mut workflow = ModelDiscoveryWorkflow::new();
    workflow.begin(RefreshStrategy::OnlineIfUncached);
    workflow.record_cache_miss();
    workflow.record_fetch_started();
    workflow.record_live_catalog();
    assert_eq!(workflow.state(), ModelDiscoveryState::LiveCatalog);

    let mut workflow = ModelDiscoveryWorkflow::new();
    workflow.begin(RefreshStrategy::Online);
    workflow.record_unsupported_catalog();
    assert_eq!(workflow.state(), ModelDiscoveryState::UnsupportedCatalog);
}
