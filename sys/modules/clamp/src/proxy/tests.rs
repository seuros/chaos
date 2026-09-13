#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use futures::StreamExt;
use std::sync::Mutex;

/// Captures exchanges in memory for assertions.
#[derive(Default)]
struct TestSink {
    recorded: Mutex<Vec<WiretapExchange>>,
}

impl TestSink {
    fn len(&self) -> usize {
        self.recorded.lock().unwrap().len()
    }
    fn one(&self) -> WiretapExchange {
        let guard = self.recorded.lock().unwrap();
        assert_eq!(guard.len(), 1, "expected exactly one exchange");
        guard[0].clone()
    }
}

impl WiretapSink for TestSink {
    fn record(&self, exchange: WiretapExchange) {
        self.recorded.lock().unwrap().push(exchange);
    }
}

fn data_stream(chunks: Vec<&'static str>) -> BodyDataStream {
    let items = chunks
        .into_iter()
        .map(|c| Ok::<Bytes, BoxError>(Bytes::from(c)));
    Body::from_stream(futures::stream::iter(items)).into_data_stream()
}

fn parts() -> RecordParts {
    RecordParts {
        method: "POST".to_string(),
        path: "/v1/messages".to_string(),
        headers: json!({}),
        request: None,
    }
}

async fn drain(tee: &mut TeeBody) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(item) = tee.next().await {
        out.extend_from_slice(&item.unwrap());
    }
    out
}

#[test]
fn redacts_sensitive_headers() {
    let mut headers = rama::http::HeaderMap::new();
    headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
    headers.insert("x-api-key", HeaderValue::from_static("sk-ant-123"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));

    let json = redact_headers(&headers);
    assert_eq!(json["authorization"], REDACTED);
    assert_eq!(json["x-api-key"], REDACTED);
    assert_eq!(json["content-type"], "application/json");
}

#[tokio::test]
async fn tee_passes_through_and_records_full_body() {
    let sink = Arc::new(TestSink::default());
    let mut tee = TeeBody::new(
        data_stream(vec!["event: a\n", "data: b\n", "\n"]),
        parts(),
        200,
        sink.clone(),
        1024,
    );
    // Forwarded bytes must be byte-identical to the upstream stream.
    assert_eq!(drain(&mut tee).await, b"event: a\ndata: b\n\n");
    let rec = sink.one();
    assert_eq!(rec.status, Some(200));
    assert_eq!(rec.response.as_deref(), Some("event: a\ndata: b\n\n"));
    assert!(!rec.response_truncated);
}

#[tokio::test]
async fn tee_truncates_capture_but_forwards_everything() {
    let sink = Arc::new(TestSink::default());
    // Tiny cap: total payload (15 bytes) exceeds it.
    let mut tee = TeeBody::new(
        data_stream(vec!["12345", "67890", "abcde"]),
        parts(),
        200,
        sink.clone(),
        8,
    );
    // Client still receives the COMPLETE body despite capture truncation.
    assert_eq!(drain(&mut tee).await, b"1234567890abcde");
    let rec = sink.one();
    assert!(
        rec.response_truncated,
        "capture should be flagged truncated"
    );
    let captured = rec.response.unwrap();
    assert!(
        captured.len() <= 8,
        "captured {} bytes over cap",
        captured.len()
    );
}

#[tokio::test]
async fn tee_records_on_early_drop() {
    let sink = Arc::new(TestSink::default());
    let mut tee = TeeBody::new(
        data_stream(vec!["chunk1", "chunk2", "chunk3"]),
        parts(),
        200,
        sink.clone(),
        1024,
    );
    // Consume one chunk, then drop mid-stream (client disconnect).
    let first = tee.next().await.unwrap().unwrap();
    assert_eq!(&first[..], b"chunk1");
    drop(tee);
    // The partial turn is still recorded exactly once.
    assert_eq!(sink.len(), 1);
    assert_eq!(sink.one().response.as_deref(), Some("chunk1"));
}

// --- Full-proxy round trips against a mock upstream ---

/// Spawn a mock upstream: `/error` → 500, everything else → 200 SSE.
async fn start_mock_upstream() -> u16 {
    let exec = Executor::default();
    let listener = TcpListener::build(exec.clone())
        .bind_address("127.0.0.1:0")
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let svc = HttpServer::auto(exec).service(Arc::new(service_fn(|req: Request| async move {
        let resp = if req
            .uri()
            .path()
            .is_some_and(|p| p.as_encoded_str() == "/error")
        {
            error_response(StatusCode::INTERNAL_SERVER_ERROR)
        } else {
            let mut r = Response::new(Body::from("event: message_start\ndata: {}\n\n"));
            *r.status_mut() = StatusCode::OK;
            r
        };
        Ok::<_, std::convert::Infallible>(resp)
    })));
    tokio::spawn(async move {
        listener.serve(svc).await;
    });
    port
}

async fn post(port: u16, path: &str, body: &'static str) -> (u16, String) {
    let tls = TlsClientConfig::new().with_alpn_http_auto();
    let client = (MapResponseBodyLayer::new_boxed_streaming_body(),).into_layer(
        EasyHttpWebClient::connector_builder()
            .with_default_transport_connector()
            .with_default_dns_connector()
            .with_tls_proxy_support_using_rustls()
            .with_proxy_support()
            .with_tls_support_using_rustls_and_default_http_version(tls, Version::HTTP_11)
            .with_default_http_connector(Executor::default())
            .without_connection_pool()
            .build_client(),
    );
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://127.0.0.1:{port}{path}"))
        .header("authorization", "Bearer sekret")
        .body(Body::from(body))
        .unwrap();
    let resp = client.serve(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn global_egress_routes_claude_code_through_gateway() {
    let exec = Executor::default();
    let listener = TcpListener::build(exec.clone())
        .bind_address("127.0.0.1:0")
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let http = HttpServer::auto(exec).service(Arc::new(service_fn(|req: Request| async move {
        assert_eq!(
            req.uri().request_target(),
            "/egress/chaos/v1/messages?beta=true"
        );
        assert_eq!(req.headers()["x-lsd-upstream"], "https://api.anthropic.com");
        assert_eq!(req.headers()["authorization"], "Bearer sekret");
        assert_eq!(req.into_body().collect().await.unwrap().to_bytes(), "{}");
        Ok::<_, std::convert::Infallible>(
            Response::builder()
                .status(402)
                .body(Body::from("balance exhausted"))
                .unwrap(),
        )
    })));
    let gateway = tokio::spawn(async move { listener.serve(http).await });
    let sink = Arc::new(TestSink::default());
    let proxy = WiretapProxy::start_with_egress(
        sink.clone(),
        chaos_client::Egress::parse(&format!("http://127.0.0.1:{port}/egress/chaos")).unwrap(),
    )
    .await
    .unwrap();
    let (status, body) = post(proxy.port(), "/v1/messages?beta=true", "{}").await;
    assert_eq!(status, 402);
    assert_eq!(body, "balance exhausted");
    assert_eq!(sink.one().status, Some(402));
    proxy.shutdown();
    gateway.abort();
}

#[tokio::test]
async fn proxy_records_non_json_request_body() {
    let upstream = start_mock_upstream().await;
    let sink = Arc::new(TestSink::default());
    let proxy =
        WiretapProxy::start_with_upstream(sink.clone(), &format!("http://127.0.0.1:{upstream}"))
            .await
            .unwrap();

    let (status, body) = post(proxy.port(), "/v1/messages", "this is not json {{{").await;
    assert_eq!(status, 200);
    assert!(body.contains("message_start"));

    let rec = sink.one();
    assert_eq!(rec.status, Some(200));
    assert!(rec.request.is_none(), "non-json body should record as null");
    assert_eq!(rec.headers["authorization"], REDACTED);
    proxy.shutdown();
}

#[tokio::test]
async fn proxy_records_upstream_error_status() {
    let upstream = start_mock_upstream().await;
    let sink = Arc::new(TestSink::default());
    let proxy =
        WiretapProxy::start_with_upstream(sink.clone(), &format!("http://127.0.0.1:{upstream}"))
            .await
            .unwrap();

    let (status, _) = post(proxy.port(), "/error", "{}").await;
    assert_eq!(status, 500);
    assert_eq!(sink.one().status, Some(500));
    proxy.shutdown();
}

#[tokio::test]
async fn proxy_handles_concurrent_turns() {
    let upstream = start_mock_upstream().await;
    let sink = Arc::new(TestSink::default());
    let proxy =
        WiretapProxy::start_with_upstream(sink.clone(), &format!("http://127.0.0.1:{upstream}"))
            .await
            .unwrap();
    let port = proxy.port();

    let mut handles = Vec::new();
    for _ in 0..16 {
        handles.push(tokio::spawn(async move {
            post(port, "/v1/messages", "{\"model\":\"x\"}").await
        }));
    }
    for h in handles {
        let (status, _) = h.await.unwrap();
        assert_eq!(status, 200);
    }
    // Every concurrent turn is recorded exactly once.
    assert_eq!(sink.len(), 16);
    proxy.shutdown();
}
