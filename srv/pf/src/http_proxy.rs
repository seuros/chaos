mod helpers;
mod request_handler;
mod responses;
mod tunnel;

use crate::network_policy::NetworkPolicyDecider;
use crate::state::NetworkProxyState;
use anyhow::Context as _;
use anyhow::Result;
use rama::Layer;
use rama::http::layer::remove_header::RemoveResponseHeaderLayer;
use rama::http::layer::upgrade::UpgradeLayer;
use rama::http::matcher::MethodMatcher;
use rama::http::server::HttpServer;
use rama::layer::AddInputExtensionLayer;
use rama::rt::Executor;
use rama::service::service_fn;
use rama::tcp::server::TcpListener;
use std::net::SocketAddr;
use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use tracing::info;

pub async fn run_http_proxy(
    state: Arc<NetworkProxyState>,
    addr: SocketAddr,
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
) -> Result<()> {
    let exec = Executor::default();
    let listener = TcpListener::build(exec)
        .bind_address(addr)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))
        .with_context(|| format!("bind HTTP proxy: {addr}"))?;

    run_http_proxy_with_listener(state, listener, policy_decider).await
}

pub async fn run_http_proxy_with_std_listener(
    state: Arc<NetworkProxyState>,
    listener: StdTcpListener,
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
) -> Result<()> {
    let exec = Executor::default();
    let listener = TcpListener::try_from_std_tcp_listener(listener, exec)
        .context("convert std listener to HTTP proxy listener")?;
    run_http_proxy_with_listener(state, listener, policy_decider).await
}

async fn run_http_proxy_with_listener(
    state: Arc<NetworkProxyState>,
    listener: TcpListener,
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
) -> Result<()> {
    let addr = listener
        .local_addr()
        .context("read HTTP proxy listener local addr")?;

    // This proxy listener only needs HTTP/1 proxy semantics. Using Rama's auto builder
    // forces every accepted socket through the HTTP version sniffing pre-read path before proxy
    // request parsing, which can stall some local clients on macOS before CONNECT/absolute-form
    // handling runs at all.
    let http_service = HttpServer::new_http1(Executor::default()).service(
        (
            UpgradeLayer::new_with_services(
                Executor::default(),
                MethodMatcher::CONNECT,
                service_fn({
                    let policy_decider = policy_decider.clone();
                    move |req| request_handler::http_connect_accept(policy_decider.clone(), req)
                }),
                service_fn(tunnel::http_connect_proxy),
            ),
            RemoveResponseHeaderLayer::hop_by_hop(),
        )
            .into_layer(service_fn({
                let policy_decider = policy_decider.clone();
                move |req| request_handler::http_plain_proxy(policy_decider.clone(), req)
            })),
    );

    info!("HTTP proxy listening on {addr}");

    listener
        .serve(AddInputExtensionLayer::new_arc(state).into_layer(http_service))
        .await;
    Ok(())
}

#[cfg(test)]
mod tests;
