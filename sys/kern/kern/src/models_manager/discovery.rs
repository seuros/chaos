use super::RefreshStrategy;
use super::manager::ModelsManager;
use crate::error::{ChaosErr, Result as CoreResult};
use state_machines::runtime::{Clock, Runner};
use state_machines::state_machine;

#[derive(Debug)]
pub(super) struct DiscoveryContext {
    manager: ModelsManager,
    strategy: RefreshStrategy,
}

state_machine! {
    name: ModelDiscovery,
    context: DiscoveryContext,
    dynamic: true,
    initial: Idle,
    states: [
        Idle, CheckingCache, CacheMiss, CachedCatalog, Fetching,
        LiveCatalog, UnsupportedCatalog, Failed(Option<ChaosErr>)
    ],
    runtime: {
        CheckingCache { invoke: [inspect_cache] }
        Fetching { invoke: [fetch] }
    },
    events {
        begin {
            branching: true,
            transition: { from: Idle, to: CheckingCache, guards: [use_cache] }
            transition: { from: Idle, to: Fetching, guards: [online] }
        }
        cache_hit { transition: { from: CheckingCache, to: CachedCatalog } }
        cache_miss {
            branching: true,
            transition: { from: CheckingCache, to: CacheMiss, guards: [offline] }
            transition: { from: CheckingCache, to: Fetching, guards: [allow_fetch] }
        }
        fetched_live { transition: { from: Fetching, to: LiveCatalog } }
        fetched_unsupported { transition: { from: Fetching, to: UnsupportedCatalog } }
        fetch_failed {
            payload: Option<ChaosErr>,
            transition: { from: Fetching, to: Failed, data: own_error }
        }
    }
}

impl<S> ModelDiscovery<S> {
    fn use_cache(&self, ctx: &DiscoveryContext) -> bool {
        ctx.strategy != RefreshStrategy::Online
    }

    fn online(&self, ctx: &DiscoveryContext) -> bool {
        ctx.strategy == RefreshStrategy::Online
    }

    fn offline(&self, ctx: &DiscoveryContext) -> bool {
        ctx.strategy == RefreshStrategy::Offline
    }

    fn allow_fetch(&self, ctx: &DiscoveryContext) -> bool {
        !self.offline(ctx)
    }

    fn own_error(&self, error: &mut Option<ChaosErr>) -> Option<ChaosErr> {
        Some(
            error
                .take()
                .unwrap_or_else(|| unreachable!("catalog failure carries its error")),
        )
    }

    fn inspect_cache(&self) -> impl Future<Output = ModelDiscoveryEvent> + Send + 'static {
        let manager = self.ctx.manager.clone();
        async move {
            if let Some(cache) = manager.load_fresh_cache().await {
                manager.apply_cache_entry(cache).await;
                ModelDiscoveryEvent::CacheHit
            } else {
                ModelDiscoveryEvent::CacheMiss
            }
        }
    }

    fn fetch(&self) -> impl Future<Output = ModelDiscoveryEvent> + Send + 'static {
        let manager = self.ctx.manager.clone();
        async move {
            match manager.fetch_and_update_models().await {
                Ok(true) => ModelDiscoveryEvent::FetchedLive,
                Ok(false) => ModelDiscoveryEvent::FetchedUnsupported,
                Err(error) => ModelDiscoveryEvent::FetchFailed(Some(error)),
            }
        }
    }
}

struct DiscoveryClock;

impl Clock for DiscoveryClock {
    fn now(&self) -> u64 {
        0
    }
}

pub(super) fn refresh(
    manager: ModelsManager,
    strategy: RefreshStrategy,
) -> std::pin::Pin<Box<dyn Future<Output = CoreResult<()>> + Send>> {
    Box::pin(run(manager, strategy))
}

async fn run(manager: ModelsManager, strategy: RefreshStrategy) -> CoreResult<()> {
    let mut machine = DynamicModelDiscovery::new(DiscoveryContext { manager, strategy });
    machine
        .handle(ModelDiscoveryEvent::Begin)
        .map_err(|_| ChaosErr::InternalServerError)?;
    let mut runner = Runner::new(machine, 4);
    runner
        .start(&DiscoveryClock)
        .map_err(|_| ChaosErr::InternalServerError)?;
    loop {
        runner
            .drain(8)
            .await
            .map_err(|_| ChaosErr::InternalServerError)?;
        match runner.machine().current_state() {
            ModelDiscoveryState::CheckingCache | ModelDiscoveryState::Fetching => {
                runner
                    .wait_for_work()
                    .await
                    .map_err(|_| ChaosErr::InternalServerError)?;
            }
            ModelDiscoveryState::Failed => {
                let mut machine = runner.into_machine();
                return Err(machine
                    .failed_data_mut()
                    .and_then(Option::take)
                    .unwrap_or_else(|| unreachable!("catalog failure")));
            }
            _ => return Ok(()),
        }
    }
}
