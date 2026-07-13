use crate::refresh_strategy::RefreshStrategy;
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

#[derive(Debug)]
pub struct ModelDiscoveryWorkflow {
    machine: DynamicModelDiscovery<()>,
}

impl ModelDiscoveryWorkflow {
    pub fn new() -> Self {
        Self {
            machine: DynamicModelDiscovery::new(()),
        }
    }

    pub fn begin(&mut self, strategy: RefreshStrategy) {
        match strategy {
            RefreshStrategy::Offline | RefreshStrategy::OnlineIfUncached => {
                let _ = self.machine.handle(ModelDiscoveryEvent::InspectCache);
            }
            RefreshStrategy::Online => {
                let _ = self.machine.handle(ModelDiscoveryEvent::Fetch);
            }
        }
    }

    pub fn record_cache_hit(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::CacheHit);
    }

    pub fn record_cache_miss(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::CacheMiss);
    }

    pub fn record_fetch_started(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::Fetch);
    }

    pub fn record_live_catalog(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::FetchedLive);
    }

    pub fn record_unsupported_catalog(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::FetchedUnsupported);
    }

    pub fn record_failed(&mut self) {
        let _ = self.machine.handle(ModelDiscoveryEvent::FetchFailed);
    }

    #[cfg(test)]
    pub fn state(&self) -> ModelDiscoveryState {
        self.machine.current_state()
    }
}

impl Default for ModelDiscoveryWorkflow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
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
}
