use super::*;
use crate::config::NetworkMode;
use crate::config::NetworkProxyConfig;
use crate::config::NetworkProxySettings;
use crate::network_policy::test_support::POLICY_DECISION_EVENT_NAME;
use crate::network_policy::test_support::capture_events;
use crate::network_policy::test_support::find_event_by_name;
use crate::runtime::ConfigReloader;
use crate::runtime::ConfigState;
use crate::state::NetworkProxyConstraints;
use crate::state::build_config_state;
use pretty_assertions::assert_eq;
use rama::extensions::Extensions;

use rama::net::address::HostWithPort;
use rama::net::address::SocketAddress;
use rama::proxy::socks5::server::udp::RelayDirection;
use std::future::Future;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone)]
struct StaticReloader {
    state: ConfigState,
}

impl ConfigReloader for StaticReloader {
    fn maybe_reload(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<ConfigState>>> + Send + '_>> {
        Box::pin(async { Ok(None) })
    }

    fn reload_now(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<ConfigState>> + Send + '_>> {
        let state = self.state.clone();
        Box::pin(async move { Ok(state) })
    }

    fn source_label(&self) -> String {
        "static test reloader".to_string()
    }
}

fn state_for_settings(network: NetworkProxySettings) -> Arc<NetworkProxyState> {
    let config = NetworkProxyConfig { network };
    let state = build_config_state(config, NetworkProxyConstraints::default()).unwrap();
    let reloader = Arc::new(StaticReloader {
        state: state.clone(),
    });
    Arc::new(NetworkProxyState::with_reloader(state, reloader))
}

#[tokio::test(flavor = "current_thread")]
async fn handle_socks5_tcp_emits_block_decision_for_proxy_disabled() {
    let state = state_for_settings(NetworkProxySettings {
        enabled: false,
        mode: NetworkMode::Full,
        ..NetworkProxySettings::default()
    });
    let request =
        TcpRequest::new(HostWithPort::try_from("example.com:443").expect("valid authority"));
    request.extensions().insert_arc(state.clone());

    let (result, events) = capture_events(|| async {
        handle_socks5_tcp(request, TcpConnector::default(), None).await
    })
    .await;
    assert!(result.is_err(), "proxy-disabled request should be denied");

    let event = find_event_by_name(&events, POLICY_DECISION_EVENT_NAME)
        .expect("expected policy decision event");
    assert_eq!(event.field("network.policy.scope"), Some("non_domain"));
    assert_eq!(event.field("network.policy.decision"), Some("deny"));
    assert_eq!(event.field("network.policy.source"), Some("proxy_state"));
    assert_eq!(
        event.field("network.policy.reason"),
        Some(REASON_PROXY_DISABLED)
    );
    assert_eq!(
        event.field("network.transport.protocol"),
        Some("socks5_tcp")
    );
    assert_eq!(event.field("server.address"), Some("example.com"));
    assert_eq!(event.field("server.port"), Some("443"));
    assert_eq!(event.field("http.request.method"), Some("none"));
    assert_eq!(event.field("client.address"), Some("unknown"));
}

#[tokio::test(flavor = "current_thread")]
async fn inspect_socks5_udp_emits_block_decision_for_mode_guard_deny() {
    let state = state_for_settings(NetworkProxySettings {
        enabled: true,
        mode: NetworkMode::Limited,
        ..NetworkProxySettings::default()
    });
    let request = RelayRequest {
        direction: RelayDirection::South,
        server_address: SocketAddress::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 53),
        payload: Default::default(),
        extensions: Extensions::new(),
    };

    let (result, events) =
        capture_events(|| async { inspect_socks5_udp(request, state, None).await }).await;
    assert!(result.is_err(), "limited-mode UDP request should be denied");

    let event = find_event_by_name(&events, POLICY_DECISION_EVENT_NAME)
        .expect("expected policy decision event");
    assert_eq!(event.field("network.policy.scope"), Some("non_domain"));
    assert_eq!(event.field("network.policy.decision"), Some("deny"));
    assert_eq!(event.field("network.policy.source"), Some("mode_guard"));
    assert_eq!(
        event.field("network.policy.reason"),
        Some(REASON_METHOD_NOT_ALLOWED)
    );
    assert_eq!(
        event.field("network.transport.protocol"),
        Some("socks5_udp")
    );
    assert_eq!(event.field("server.address"), Some("93.184.216.34"));
    assert_eq!(event.field("server.port"), Some("53"));
    assert_eq!(event.field("http.request.method"), Some("none"));
    assert_eq!(event.field("client.address"), Some("unknown"));
}
