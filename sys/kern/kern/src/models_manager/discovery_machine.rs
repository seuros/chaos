use crate::models_manager::manager::RefreshStrategy;
use state_machines::state_machine;

state_machine! {
    name: ModelDiscovery,
    dynamic: true,
    initial: Idle,
    states: [
        Idle,
        CheckingCache,
        CacheMiss,
        CachedCatalog,
        Fetching,
        LiveCatalog,
        UnsupportedCatalog,
        Failed
    ],
    events {
        inspect_cache {
            transition: { from: Idle, to: CheckingCache }
        }
        cache_hit {
            transition: { from: CheckingCache, to: CachedCatalog }
        }
        cache_miss {
            transition: { from: CheckingCache, to: CacheMiss }
        }
        fetch {
            transition: { from: Idle, to: Fetching }
            transition: { from: CacheMiss, to: Fetching }
            transition: { from: CachedCatalog, to: Fetching }
            transition: { from: LiveCatalog, to: Fetching }
            transition: { from: UnsupportedCatalog, to: Fetching }
            transition: { from: Failed, to: Fetching }
        }
        fetched_live {
            transition: { from: Fetching, to: LiveCatalog }
        }
        fetched_unsupported {
            transition: { from: Fetching, to: UnsupportedCatalog }
        }
        fetch_failed {
            transition: { from: Fetching, to: Failed }
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelDiscoveryState {
    Idle,
    CheckingCache,
    CacheMiss,
    CachedCatalog,
    Fetching,
    LiveCatalog,
    UnsupportedCatalog,
    Failed,
}

#[derive(Debug)]
pub(crate) struct ModelDiscoveryWorkflow {
    machine: DynamicModelDiscovery<()>,
}

impl ModelDiscoveryWorkflow {
    pub(crate) fn new() -> Self {
        Self {
            machine: DynamicModelDiscovery::new(()),
        }
    }

    pub(crate) fn begin(&mut self, strategy: RefreshStrategy) {
        match strategy {
            RefreshStrategy::Offline | RefreshStrategy::OnlineIfUncached => {
                let _ = self.machine.handle(ModelDiscoveryEvent::InspectCache);
            }
            RefreshStrategy::Online => {
                let _ = self.machine.handle(ModelDiscoveryEvent::Fetch);
            }
        }
    }

    pub(crate) fn record_cache_hit(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::CacheHit);
    }

    pub(crate) fn record_cache_miss(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::CacheMiss);
    }

    pub(crate) fn record_fetch_started(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::Fetch);
    }

    pub(crate) fn record_live_catalog(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::FetchedLive);
    }

    pub(crate) fn record_unsupported_catalog(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::FetchedUnsupported);
    }

    pub(crate) fn record_failed(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::FetchFailed);
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> ModelDiscoveryState {
        match self.machine.current_state() {
            "Idle" => ModelDiscoveryState::Idle,
            "CheckingCache" => ModelDiscoveryState::CheckingCache,
            "CacheMiss" => ModelDiscoveryState::CacheMiss,
            "CachedCatalog" => ModelDiscoveryState::CachedCatalog,
            "Fetching" => ModelDiscoveryState::Fetching,
            "LiveCatalog" => ModelDiscoveryState::LiveCatalog,
            "UnsupportedCatalog" => ModelDiscoveryState::UnsupportedCatalog,
            "Failed" => ModelDiscoveryState::Failed,
            _ => ModelDiscoveryState::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_refresh_starts_with_cache_lookup() {
        let mut workflow = ModelDiscoveryWorkflow::new();

        workflow.begin(RefreshStrategy::Offline);

        assert_eq!(workflow.state(), ModelDiscoveryState::CheckingCache);
    }

    #[test]
    fn online_uncached_cache_hit_stops_at_cached_catalog() {
        let mut workflow = ModelDiscoveryWorkflow::new();
        workflow.begin(RefreshStrategy::OnlineIfUncached);

        workflow.record_cache_hit();

        assert_eq!(workflow.state(), ModelDiscoveryState::CachedCatalog);
    }

    #[test]
    fn online_uncached_cache_miss_flows_into_live_catalog() {
        let mut workflow = ModelDiscoveryWorkflow::new();
        workflow.begin(RefreshStrategy::OnlineIfUncached);

        workflow.record_cache_miss();
        workflow.record_fetch_started();
        workflow.record_live_catalog();

        assert_eq!(workflow.state(), ModelDiscoveryState::LiveCatalog);
    }

    #[test]
    fn online_fetch_can_land_in_unsupported_catalog() {
        let mut workflow = ModelDiscoveryWorkflow::new();
        workflow.begin(RefreshStrategy::Online);

        workflow.record_unsupported_catalog();

        assert_eq!(workflow.state(), ModelDiscoveryState::UnsupportedCatalog);
    }
}
