use std::time::Duration;

use rama::Layer;
use rama::Service;
use rama::extensions::ExtensionsRef;
use rama::http::Body;
use rama::http::Request;
use rama::http::body::util::BodyExt;
use rama::http::core::client::conn::http1;
use rama::net::client::NoProxyEnvLayer;
use rama::net::client::ProxyEnvLayer;
use rama::net::client::ProxyRoute;
use rama::service::service_fn;
use url::Url;

/// Plain HTTP fixtures only. Send an absolute-form GET to HTTP proxies, rather
/// than a CONNECT tunnel: network approval tests must exercise the HTTP lane.
pub fn get(
    url: &str,
    proxy: bool,
    timeout: Duration,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::time::timeout(timeout, async {
            let target = Url::parse(url)?;
            if target.scheme() != "http" {
                return Err("process fixtures support only plaintext HTTP".into());
            }
            let request = Request::builder().uri(url).body(Body::empty())?;
            let mut request = if proxy {
                NoProxyEnvLayer::default()
                    .into_layer(
                        ProxyEnvLayer::default()
                            .with_http_proxy_env_vars(["http_proxy", "HTTP_PROXY"])
                            .into_layer(service_fn(|request: Request| async {
                                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(request)
                            })),
                    )
                    .serve(request)
                    .await?
            } else {
                request
            };
            let route = request.extensions().get_ref::<ProxyRoute>();
            let stream = if let Some(ProxyRoute::Proxy(proxy)) = route {
                if proxy
                    .protocol
                    .as_ref()
                    .is_some_and(|protocol| protocol.as_str() != "http")
                {
                    return Err("process fixtures support only HTTP proxies".into());
                }
                tokio::net::TcpStream::connect(proxy.address.to_string()).await?
            } else {
                let host = target.host_str().ok_or("HTTP URL requires a host")?;
                let port = target
                    .port_or_known_default()
                    .ok_or("HTTP URL requires a port")?;
                let stream = tokio::net::TcpStream::connect((host, port)).await?;
                *request.uri_mut() = request.uri().request_target().parse()?;
                stream
            };
            let authority = &target[url::Position::BeforeHost..url::Position::AfterPort];
            request.headers_mut().insert("host", authority.parse()?);
            request.headers_mut().insert("connection", "close".parse()?);
            let (mut sender, connection) =
                http1::handshake(rama::tcp::TcpStream::new(stream)).await?;
            let connection = tokio::spawn(connection);
            let response = sender.send_request(request).await?;
            if !response.status().is_success() {
                connection.abort();
                return Err(format!("HTTP {}", response.status()).into());
            }
            let body = response.into_body().collect().await?.to_bytes();
            connection.await??;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(String::from_utf8(body.to_vec())?)
        })
        .await?
    })
}
