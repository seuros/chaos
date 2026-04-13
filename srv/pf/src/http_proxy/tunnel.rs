#[cfg(target_os = "macos")]
use super::helpers::remove_hop_by_hop_request_headers;
use crate::mitm;
use crate::policy::normalize_host;
use crate::state::NetworkProxyState;
#[cfg(target_os = "macos")]
use crate::upstream::UpstreamClient;
use crate::upstream::proxy_for_connect;
#[cfg(target_os = "macos")]
use anyhow::Context as _;
use anyhow::Result;
use rama::Layer;
use rama::Service;
use rama::error::BoxError;
use rama::error::ErrorExt as _;
use rama::extensions::ExtensionsRef;
use rama::http::Request;
use rama::http::Response;
use rama::http::client::proxy::layer::HttpProxyConnector;
use rama::http::layer::upgrade::Upgraded;
use rama::net::Protocol;
use rama::net::address::ProxyAddress;
use rama::net::client::ConnectorService;
use rama::net::client::EstablishedClientConnection;
use rama::net::proxy::IoForwardService;
use rama::net::proxy::ProxyTarget;
use rama::rt::Executor;
use rama::tcp::client::Request as TcpRequest;
use rama::tcp::client::service::TcpConnector;
use rama::tcp::proxy::IoToProxyBridgeIoLayer;
use rama::tls::rustls::client::TlsConnectorDataBuilder;
use rama::tls::rustls::client::TlsConnectorLayer;
use std::convert::Infallible;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::config::NetworkMode;

#[derive(Clone)]
pub(super) struct HttpsTunnelConnector<C> {
    pub(super) inner: C,
}

impl<C> Service<TcpRequest> for HttpsTunnelConnector<C>
where
    C: ConnectorService<TcpRequest> + Clone,
{
    type Output = EstablishedClientConnection<C::Connection, TcpRequest>;
    type Error = C::Error;

    async fn serve(&self, req: TcpRequest) -> Result<Self::Output, Self::Error> {
        self.inner.connect(req.with_protocol(Protocol::HTTPS)).await
    }
}

pub(super) async fn http_connect_proxy(upgraded: Upgraded) -> Result<(), Infallible> {
    let mode = upgraded
        .extensions()
        .get_ref::<NetworkMode>()
        .copied()
        .unwrap_or(NetworkMode::Full);

    let Some(target) = upgraded
        .extensions()
        .get_ref::<ProxyTarget>()
        .map(|t| t.0.clone())
    else {
        warn!("CONNECT missing proxy target");
        return Ok(());
    };

    if mode == NetworkMode::Limited && upgraded.extensions().get_arc::<mitm::MitmState>().is_some()
    {
        let host = normalize_host(&target.host.to_string());
        let port = target.port;
        info!("CONNECT MITM enabled (host={host}, port={port}, mode={mode:?})");
        if let Err(err) = mitm::mitm_tunnel(upgraded).await {
            warn!("MITM tunnel error: {err}");
        }
        return Ok(());
    }

    let allow_upstream_proxy = match upgraded.extensions().get_arc::<NetworkProxyState>() {
        Some(state) => match state.allow_upstream_proxy().await {
            Ok(allowed) => allowed,
            Err(err) => {
                error!("failed to read upstream proxy setting: {err}");
                false
            }
        },
        None => {
            error!("missing app state");
            false
        }
    };

    let proxy = if allow_upstream_proxy {
        proxy_for_connect()
    } else {
        None
    };

    if let Err(err) = forward_connect_tunnel(upgraded, proxy).await {
        warn!("tunnel error: {err}");
    }
    Ok(())
}

pub(super) async fn forward_connect_tunnel(
    upgraded: Upgraded,
    proxy: Option<ProxyAddress>,
) -> Result<(), BoxError> {
    let authority = upgraded
        .extensions()
        .get_ref::<ProxyTarget>()
        .map(|target| target.0.clone())
        .ok_or_else(|| BoxError::from("missing forward authority"))?;

    let proxy_connector = HttpProxyConnector::optional(TcpConnector::new(Executor::default()));
    let tls_config = TlsConnectorDataBuilder::new()
        .with_alpn_protocols_http_auto()
        .build();
    let connector = TlsConnectorLayer::tunnel(None)
        .with_connector_data(tls_config)
        .into_layer(proxy_connector);
    let connector = HttpsTunnelConnector { inner: connector };
    if let Some(proxy) = proxy {
        upgraded.extensions().insert(proxy);
    }
    IoToProxyBridgeIoLayer::extension_proxy_target_with_connector(connector)
        .into_layer(IoForwardService::new())
        .serve(upgraded)
        .await
        .map_err(|err| err.with_context(|| format!("forward CONNECT tunnel to {authority}")))
}

pub(super) async fn proxy_via_unix_socket(req: Request, socket_path: &str) -> Result<Response> {
    #[cfg(target_os = "macos")]
    {
        let client = UpstreamClient::unix_socket(socket_path);

        let (mut parts, body) = req.into_parts();
        let path = parts
            .uri
            .path_and_query()
            .map(rama::http::uri::PathAndQuery::as_str)
            .unwrap_or("/");
        parts.uri = path
            .parse()
            .with_context(|| format!("invalid unix socket request path: {path}"))?;
        parts.headers.remove("x-unix-socket");
        remove_hop_by_hop_request_headers(&mut parts.headers);

        let req = Request::from_parts(parts, body);
        client.serve(req).await.map_err(anyhow::Error::from)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = req;
        let _ = socket_path;
        Err(anyhow::anyhow!("unix sockets not supported"))
    }
}
