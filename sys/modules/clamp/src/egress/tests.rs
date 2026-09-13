#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use rama::http::Body;
use rama::http::body::util::BodyExt;
use std::sync::Mutex;

#[derive(Default)]
struct TestSink {
    recorded: Mutex<Vec<WiretapExchange>>,
}

impl WiretapSink for TestSink {
    fn record(&self, exchange: WiretapExchange) {
        self.recorded.lock().unwrap().push(exchange);
    }
}

#[test]
fn allowlist_matches_exact_hosts_and_parses_connect_targets() {
    let policy = EgressPolicy::antigravity();

    assert!(policy.permits("cloudcode-pa.googleapis.com"));
    // Case and a trailing root dot are normalized away.
    assert!(policy.permits("CloudCode-PA.googleapis.com."));
    // Sibling endpoints on the same apex are not implied by an allowed one.
    assert!(!policy.permits("aiplatform.googleapis.com"));
    assert!(!policy.permits("play.googleapis.com"));
    // No suffix confusion: an attacker-controlled parent is not a match.
    assert!(!policy.permits("cloudcode-pa.googleapis.com.evil.test"));
    assert!(!policy.permits("evil.test"));

    assert_eq!(
        connect_host("cloudcode-pa.googleapis.com:443").as_deref(),
        Some("cloudcode-pa.googleapis.com")
    );
    assert_eq!(
        connect_host("https://oauth2.googleapis.com/token").as_deref(),
        Some("oauth2.googleapis.com")
    );
    assert_eq!(connect_host("[::1]:8443").as_deref(), Some("::1"));
    assert_eq!(connect_host(""), None);
}

#[tokio::test]
async fn global_egress_cannot_use_direct_tunnel_relay() {
    let policy = EgressPolicy::antigravity()
        .with_gateway(Some(
            chaos_client::Egress::parse("http://gateway/egress/chaos").unwrap(),
        ))
        .without_body_inspection();
    let err = EgressProxy::start(policy, Arc::new(TestSink::default()), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("global egress requires TLS inspection")
    );
}

#[tokio::test]
async fn global_egress_routes_inspected_antigravity_requests() {
    let exec = Executor::default();
    let listener = TcpListener::build(exec.clone())
        .bind_address("127.0.0.1:0")
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let http =
        HttpServer::auto(exec.clone()).service(Arc::new(service_fn(|req: Request| async move {
            assert_eq!(
                req.uri().request_target(),
                "/egress/chaos/v1internal:streamGenerateContent?alt=sse"
            );
            assert_eq!(
                req.headers()["x-lsd-upstream"],
                "https://cloudcode-pa.googleapis.com"
            );
            assert_eq!(req.headers()["authorization"], "Bearer vendor-token");
            Ok::<_, std::convert::Infallible>(
                Response::builder()
                    .status(200)
                    .body(Body::from("data: done\n\n"))
                    .unwrap(),
            )
        })));
    let gateway = tokio::spawn(async move { listener.serve(http).await });
    let state = EgressState {
        policy: EgressPolicy::antigravity().with_gateway(Some(
            chaos_client::Egress::parse(&format!("http://127.0.0.1:{port}/egress/chaos")).unwrap(),
        )),
        sink: Arc::new(TestSink::default()),
        tls: None,
        exec,
    };
    // Origin-form HTTP/1 request inside the already-terminated tunnel.
    let request = Request::builder()
        .uri("/v1internal:streamGenerateContent?alt=sse")
        .header("host", "cloudcode-pa.googleapis.com")
        .header("authorization", "Bearer vendor-token")
        .body(Body::from("{}"))
        .unwrap();
    let response = inspect_and_forward(request, state.clone()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "data: done\n\n"
    );

    let blocked = Request::builder()
        .uri("https://aiplatform.googleapis.com/v1/predict")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        inspect_and_forward(blocked, state).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    gateway.abort();
}

#[tokio::test]
async fn session_ca_is_written_as_owner_only_pem_covering_the_allowlist() {
    let dir = tempfile::tempdir().expect("temporary state directory");
    let path = dir.path().join("nested").join("egress-ca.pem");
    let sink = Arc::new(TestSink::default());

    let proxy = EgressProxy::start(EgressPolicy::antigravity(), sink, Some(path.clone()))
        .await
        .expect("start egress proxy");

    assert_eq!(proxy.ca_bundle_path(), Some(path.as_path()));
    assert_eq!(
        proxy.proxy_url(),
        format!("http://127.0.0.1:{}", proxy.port())
    );

    let pem = std::fs::read_to_string(&path).expect("read ca bundle");
    assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(pem.trim_end().ends_with("-----END CERTIFICATE-----"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "ca bundle must not be world readable");
    }

    proxy.shutdown();
}

#[tokio::test]
async fn blocked_destinations_are_refused_and_recorded() {
    let dir = tempfile::tempdir().expect("temporary state directory");
    let sink = Arc::new(TestSink::default());
    let proxy = EgressProxy::start(
        EgressPolicy::antigravity(),
        sink.clone(),
        Some(dir.path().join("ca.pem")),
    )
    .await
    .expect("start egress proxy");

    // Raw CONNECT to a host outside the allowlist: refused before a tunnel
    // exists, so the subprocess never gets bytes to or from the upstream.
    let status = connect_status(proxy.port(), "aiplatform.googleapis.com:443").await;
    assert_eq!(status, 403);

    // A plaintext (non-CONNECT) request is refused for the same reason.
    let status = connect_status(proxy.port(), "http://evil.test/exfil").await;
    assert_eq!(status, 403);

    let recorded = sink.recorded.lock().unwrap();
    assert_eq!(recorded.len(), 2, "both attempts recorded");
    assert!(recorded.iter().any(|e| e.path.contains("aiplatform")));
    assert!(recorded.iter().any(|e| e.path.contains("evil.test")));
    drop(recorded);

    proxy.shutdown();
}

/// Sends a bare request line to the proxy and returns the status code.
/// An absolute-form target is sent as `GET`, a `host:port` as `CONNECT`.
async fn connect_status(port: u16, target: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let method = if target.contains("://") {
        "GET"
    } else {
        "CONNECT"
    };
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to proxy");
    let request = format!("{method} {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut buf = vec![0u8; 128];
    let read = stream.read(&mut buf).await.unwrap();
    let head = String::from_utf8_lossy(&buf[..read]);
    head.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

#[test]
fn session_authority_mints_a_ca_backed_leaf_covering_every_allowlisted_host() {
    let hosts: Vec<String> = ANTIGRAVITY_ALLOWED_HOSTS
        .iter()
        .map(|host| (*host).to_owned())
        .collect();
    let auth = session_authority(&hosts).expect("mint session authority");

    // The chain is leaf-first with the session CA last: `write_ca_bundle`
    // publishes that last entry as the trust root the child process pins.
    assert_eq!(auth.cert_chain.len(), 2);
    let leaf = auth.cert_chain.first().unwrap().as_ref();
    let ca = auth.cert_chain.last().unwrap().as_ref();
    assert_ne!(leaf, ca);

    // Every allowlisted name is a SAN on the single leaf, and none of them
    // leak into the CA.
    for host in &hosts {
        assert!(
            leaf.windows(host.len()).any(|w| w == host.as_bytes()),
            "leaf is missing SAN {host}"
        );
        assert!(
            !ca.windows(host.len()).any(|w| w == host.as_bytes()),
            "CA unexpectedly carries {host}"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ca.pem");
    write_ca_bundle(&path, &auth).expect("write ca bundle");
    let pem = std::fs::read_to_string(&path).unwrap();
    assert_eq!(pem, pem_encode("CERTIFICATE", ca));

    session_authority(&["not a domain".to_owned()])
        .expect_err("non-domain allowlist entry must be rejected");
}
