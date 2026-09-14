mod decision;
mod events;
mod helpers;
mod policy;
mod state;

pub use decision::HostBlockDecision;
pub use decision::HostBlockReason;
pub use events::BlockedRequest;
pub use events::BlockedRequestArgs;
pub use events::BlockedRequestObserver;
pub use helpers::NetworkProxyAuditMetadata;
pub(crate) use helpers::unix_socket_permissions_supported;
pub use state::ConfigReloader;
pub use state::ConfigState;
pub use state::NetworkProxyState;

#[cfg(test)]
pub(crate) use test_helpers::network_proxy_state_for_policy;

#[cfg(test)]
mod test_helpers {
    use super::*;
    use crate::config::NetworkMode;
    use crate::config::NetworkProxyConfig;
    use crate::state::NetworkProxyConstraints;
    use crate::state::build_config_state;
    use anyhow::Result;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    pub(crate) fn network_proxy_state_for_policy(
        mut network: crate::config::NetworkProxySettings,
    ) -> NetworkProxyState {
        network.enabled = true;
        network.mode = NetworkMode::Full;
        let config = NetworkProxyConfig { network };
        let state = build_config_state(config, NetworkProxyConstraints::default()).unwrap();

        NetworkProxyState::with_reloader(state, Arc::new(NoopReloader))
    }

    pub(super) struct NoopReloader;

    impl ConfigReloader for NoopReloader {
        fn source_label(&self) -> String {
            "test config state".to_string()
        }

        fn maybe_reload(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Option<ConfigState>>> + Send + '_>> {
            Box::pin(async { Ok(None) })
        }

        fn reload_now(&self) -> Pin<Box<dyn Future<Output = Result<ConfigState>> + Send + '_>> {
            Box::pin(async { Err(anyhow::anyhow!("force reload is not supported in tests")) })
        }
    }
}

#[cfg(test)]
mod tests;
