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
            UpgradeLayer::new(
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
mod tests {
    use super::*;

    use crate::config::NetworkMode;
    use crate::config::NetworkProxySettings;
    use crate::runtime::network_proxy_state_for_policy;
    use helpers::remove_hop_by_hop_request_headers;
    use helpers::validate_absolute_form_host_header;
    use pretty_assertions::assert_eq;
    use rama::extensions::ExtensionsRef;
    use rama::http::Body;
    use rama::http::HeaderMap;
    use rama::http::HeaderValue;
    use rama::http::Method;
    use rama::http::Request;
    use rama::http::StatusCode;
    use rama::http::header;
    use rama::net::http::RequestContext;
    use std::net::Ipv4Addr;
    use std::net::TcpListener as StdTcpListener;
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener as TokioTcpListener;
    use tokio::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn http_connect_accept_blocks_in_limited_mode() {
        let policy = NetworkProxySettings {
            allowed_domains: vec!["example.com".to_string()],
            ..Default::default()
        };
        let state = Arc::new(network_proxy_state_for_policy(policy));
        state.set_network_mode(NetworkMode::Limited).await.unwrap();

        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("https://example.com:443")
            .header("host", "example.com:443")
            .body(Body::empty())
            .unwrap();
        req.extensions().insert_arc(state);

        let response = request_handler::http_connect_accept(None, req)
            .await
            .unwrap_err();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers().get("x-proxy-error").unwrap(),
            "blocked-by-mitm-required"
        );
    }

    #[tokio::test]
    async fn http_connect_accept_allows_allowlisted_host_in_full_mode() {
        let policy = NetworkProxySettings {
            allowed_domains: vec!["example.com".to_string()],
            ..Default::default()
        };
        let state = Arc::new(network_proxy_state_for_policy(policy));

        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("https://example.com:443")
            .header("host", "example.com:443")
            .body(Body::empty())
            .unwrap();
        req.extensions().insert_arc(state);

        let (response, _request) = request_handler::http_connect_accept(None, req)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn http_proxy_listener_accepts_plain_http1_connect_requests() {
        let target_listener = TokioTcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("target listener should bind");
        let target_addr = target_listener
            .local_addr()
            .expect("target listener should expose local addr");
        let target_task = tokio::spawn(async move {
            let (mut stream, _) = target_listener
                .accept()
                .await
                .expect("target listener should accept");
            let mut buf = [0_u8; 1];
            let _ = timeout(Duration::from_secs(1), stream.read(&mut buf)).await;
        });

        let state = Arc::new(network_proxy_state_for_policy(NetworkProxySettings {
            allowed_domains: vec!["127.0.0.1".to_string()],
            allow_local_binding: true,
            ..NetworkProxySettings::default()
        }));
        let listener =
            StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("proxy listener should bind");
        let proxy_addr = listener
            .local_addr()
            .expect("proxy listener should expose local addr");
        let proxy_task = tokio::spawn(run_http_proxy_with_std_listener(state, listener, None));

        let mut stream = tokio::net::TcpStream::connect(proxy_addr)
            .await
            .expect("client should connect to proxy");
        let request = format!(
            "CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n",
            port = target_addr.port()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("client should write CONNECT request");

        let mut buf = [0_u8; 256];
        let bytes_read = timeout(Duration::from_secs(2), stream.read(&mut buf))
            .await
            .expect("proxy should respond before timeout")
            .expect("client should read proxy response");
        let response = String::from_utf8_lossy(&buf[..bytes_read]);
        assert!(
            response.starts_with("HTTP/1.1 200 OK\r\n"),
            "unexpected proxy response: {response:?}"
        );

        drop(stream);
        proxy_task.abort();
        let _ = proxy_task.await;
        target_task.abort();
        let _ = target_task.await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_plain_proxy_blocks_unix_socket_when_method_not_allowed() {
        let state = Arc::new(network_proxy_state_for_policy(
            NetworkProxySettings::default(),
        ));
        state
            .set_network_mode(NetworkMode::Limited)
            .await
            .expect("network mode should update");

        let req = Request::builder()
            .method(Method::POST)
            .uri("http://example.com")
            .header("x-unix-socket", "/tmp/test.sock")
            .body(Body::empty())
            .expect("request should build");
        req.extensions().insert_arc(state);

        let response = request_handler::http_plain_proxy(None, req).await.unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers().get("x-proxy-error").unwrap(),
            "blocked-by-method-policy"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_plain_proxy_rejects_unix_socket_when_not_allowlisted() {
        let state = Arc::new(network_proxy_state_for_policy(
            NetworkProxySettings::default(),
        ));

        let req = Request::builder()
            .method(Method::GET)
            .uri("http://example.com")
            .header("x-unix-socket", "/tmp/test.sock")
            .body(Body::empty())
            .expect("request should build");
        req.extensions().insert_arc(state);

        let response = request_handler::http_plain_proxy(None, req).await.unwrap();

        if cfg!(target_os = "macos") {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(
                response.headers().get("x-proxy-error").unwrap(),
                "blocked-by-allowlist"
            );
        } else {
            assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "current_thread")]
    async fn http_plain_proxy_attempts_allowed_unix_socket_proxy() {
        let state = Arc::new(network_proxy_state_for_policy(NetworkProxySettings {
            allow_unix_sockets: vec!["/tmp/test.sock".to_string()],
            ..NetworkProxySettings::default()
        }));

        let req = Request::builder()
            .method(Method::GET)
            .uri("http://example.com")
            .header("x-unix-socket", "/tmp/test.sock")
            .body(Body::empty())
            .expect("request should build");
        req.extensions().insert_arc(state);

        let response = request_handler::http_plain_proxy(None, req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn http_connect_accept_denies_denylisted_host() {
        let policy = NetworkProxySettings {
            allowed_domains: vec!["**.openai.com".to_string()],
            denied_domains: vec!["api.openai.com".to_string()],
            ..Default::default()
        };
        let state = Arc::new(network_proxy_state_for_policy(policy));

        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("https://api.openai.com:443")
            .header("host", "api.openai.com:443")
            .body(Body::empty())
            .unwrap();
        req.extensions().insert_arc(state);

        let response = request_handler::http_connect_accept(None, req)
            .await
            .unwrap_err();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers().get("x-proxy-error").unwrap(),
            "blocked-by-denylist"
        );
    }

    #[tokio::test]
    async fn http_plain_proxy_rejects_absolute_uri_host_header_mismatch() {
        let state = Arc::new(network_proxy_state_for_policy(
            NetworkProxySettings::default(),
        ));
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://raw.githubusercontent.com/seuros/codex/main/README.md")
            .header(header::HOST, "api.github.com")
            .body(Body::empty())
            .unwrap();
        req.extensions().insert_arc(state);

        let response = request_handler::http_plain_proxy(None, req).await;
        assert_eq!(response.unwrap().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_absolute_form_host_header_allows_matching_default_port() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://example.com/")
            .header("host", "example.com")
            .body(Body::empty())
            .unwrap();

        assert_eq!(
            validate_absolute_form_host_header(&req, &RequestContext::try_from(&req).unwrap(),),
            Ok(())
        );
    }

    #[test]
    fn validate_absolute_form_host_header_rejects_mismatched_host() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://raw.githubusercontent.com/")
            .header("host", "api.github.com")
            .body(Body::empty())
            .unwrap();

        assert_eq!(
            validate_absolute_form_host_header(&req, &RequestContext::try_from(&req).unwrap(),),
            Err("Host header does not match request target")
        );
    }

    #[test]
    fn validate_absolute_form_host_header_rejects_missing_non_default_port() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://example.com:8080/")
            .header("host", "example.com")
            .body(Body::empty())
            .unwrap();

        assert_eq!(
            validate_absolute_form_host_header(&req, &RequestContext::try_from(&req).unwrap(),),
            Err("Host header does not match request target")
        );
    }

    #[test]
    fn remove_hop_by_hop_request_headers_keeps_forwarding_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("x-hop, keep-alive"),
        );
        headers.insert("x-hop", HeaderValue::from_static("1"));
        headers.insert(
            header::PROXY_AUTHORIZATION,
            HeaderValue::from_static("Basic abc"),
        );
        headers.insert(
            &header::X_FORWARDED_FOR,
            HeaderValue::from_static("127.0.0.1"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("example.com"));

        remove_hop_by_hop_request_headers(&mut headers);

        assert_eq!(headers.get(header::CONNECTION), None);
        assert_eq!(headers.get("x-hop"), None);
        assert_eq!(headers.get(header::PROXY_AUTHORIZATION), None);
        assert_eq!(
            headers.get(&header::X_FORWARDED_FOR),
            Some(&HeaderValue::from_static("127.0.0.1"))
        );
        assert_eq!(
            headers.get(header::HOST),
            Some(&HeaderValue::from_static("example.com"))
        );
    }
}
