//! Wiretap proxy: a loopback HTTP reverse proxy that sits between the clamped
//! `claude` subprocess and Anthropic.
//!
//! Chaos controls `ANTHROPIC_BASE_URL`, so `claude` talks plain HTTP to this
//! loopback port; TLS lives only on the proxy -> Anthropic hop. Every request
//! is recorded to a sink (a file in dev) so we can see exactly what Claude Code
//! sends on the wire. The response streams through untouched — SSE is never
//! buffered.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use rama::{
    Layer, Service,
    bytes::Bytes,
    error::BoxError,
    futures::Stream,
    http::{
        Body, HeaderValue, Request, Response, StatusCode, Uri, Version,
        body::{BodyDataStream, util::BodyExt},
        client::EasyHttpWebClient,
        header::HOST,
        layer::{
            map_response_body::MapResponseBodyLayer,
            remove_header::{RemoveRequestHeaderLayer, RemoveResponseHeaderLayer},
        },
        server::HttpServer,
    },
    net::{Protocol, address::Authority, tls::client::TlsClientConfig},
    rt::Executor,
    service::service_fn,
    tcp::server::TcpListener,
};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

const UPSTREAM_HOST: &str = "api.anthropic.com";
const REDACTED: &str = "<redacted>";
/// Headers scrubbed before a request envelope is recorded.
const SENSITIVE_HEADERS: [&str; 3] = ["authorization", "x-api-key", "proxy-authorization"];
/// Cap on buffered response bytes per turn before the record is marked truncated.
const MAX_RESPONSE_CAPTURE: usize = 8 * 1024 * 1024;

/// A single recorded request/response pair captured by the wiretap.
#[derive(Debug, Clone)]
pub struct WiretapExchange {
    /// HTTP method (e.g. `POST`).
    pub method: String,
    /// Path with query (e.g. `/v1/messages?beta=true`).
    pub path: String,
    /// Request headers, sensitive values already redacted.
    pub headers: serde_json::Value,
    /// Parsed request body, when it was valid JSON.
    pub request: Option<serde_json::Value>,
    /// Upstream HTTP status, or `None` if the request never reached Anthropic.
    pub status: Option<u16>,
    /// Captured response body (decoded SSE text), if any.
    pub response: Option<String>,
    /// Whether the response body was truncated at the capture cap.
    pub response_truncated: bool,
}

impl WiretapExchange {
    /// Render the exchange as a single JSON object (used by the file sink).
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "method": self.method,
            "path": self.path,
            "headers": self.headers,
            "request": self.request,
            "status": self.status,
            "response": self.response,
            "response_truncated": self.response_truncated,
        })
    }
}

/// Destination for recorded wiretap exchanges. Implementations must not block —
/// `record` is called from the request hot path, so heavy work (file/DB writes)
/// should be handed to a channel or spawned task.
pub trait WiretapSink: Send + Sync + 'static {
    fn record(&self, exchange: WiretapExchange);
}

/// Where the wiretap forwards traffic. Defaults to Anthropic over HTTPS; a
/// custom upstream (used for testing or alternate endpoints) is parsed from a
/// base URL.
#[derive(Clone)]
struct Upstream {
    scheme: Protocol,
    authority: Authority,
}

impl Upstream {
    fn anthropic() -> Self {
        Self {
            scheme: Protocol::HTTPS,
            authority: Authority::from_static(UPSTREAM_HOST),
        }
    }

    fn from_base_url(base_url: &str) -> Result<Self, BoxError> {
        let uri: Uri = base_url.parse()?;
        let scheme = uri.scheme().cloned().unwrap_or(Protocol::HTTPS);
        let authority = uri
            .authority()
            .map(|authority| authority.into_owned())
            .ok_or_else(|| BoxError::from("upstream base url has no authority"))?;
        Ok(Self { scheme, authority })
    }

    fn host_header(&self) -> HeaderValue {
        HeaderValue::from_str(&self.authority.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static(UPSTREAM_HOST))
    }
}

/// A running wiretap proxy. Dropping it (or calling [`WiretapProxy::shutdown`])
/// stops the listener.
pub struct WiretapProxy {
    port: u16,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for WiretapProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WiretapProxy")
            .field("port", &self.port)
            .finish()
    }
}

impl WiretapProxy {
    /// Start a wiretap proxy on an OS-assigned loopback port, recording every
    /// exchange to `sink` and forwarding to Anthropic.
    pub async fn start(sink: Arc<dyn WiretapSink>) -> Result<Self, BoxError> {
        Self::start_with_upstream_inner(sink, Upstream::anthropic()).await
    }

    /// Like [`WiretapProxy::start`] but forwarding to a custom upstream base URL
    /// (e.g. an Anthropic-compatible endpoint, or a mock server for testing).
    pub async fn start_with_upstream(
        sink: Arc<dyn WiretapSink>,
        upstream_base_url: &str,
    ) -> Result<Self, BoxError> {
        Self::start_with_upstream_inner(sink, Upstream::from_base_url(upstream_base_url)?).await
    }

    async fn start_with_upstream_inner(
        sink: Arc<dyn WiretapSink>,
        upstream: Upstream,
    ) -> Result<Self, BoxError> {
        let exec = Executor::default();
        let listener = TcpListener::build(exec.clone())
            .bind_address("127.0.0.1:0")
            .await?;
        let port = listener.local_addr()?.port();

        let http = HttpServer::auto(exec).service(Arc::new(service_fn(move |req: Request| {
            let sink = Arc::clone(&sink);
            let upstream = upstream.clone();
            async move { forward(req, sink, upstream).await }
        })));

        let task = tokio::spawn(async move {
            listener.serve(http).await;
        });

        info!(port, "clamp wiretap proxy listening on loopback");
        Ok(Self { port, task })
    }

    /// Convenience constructor: record to a JSONL file (or tracing when `None`).
    pub async fn start_to_file(record_file: Option<PathBuf>) -> Result<Self, BoxError> {
        Self::start(Arc::new(FileWiretapSink::new(record_file))).await
    }

    /// The loopback port the proxy is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The base URL to hand to the clamped subprocess via `ANTHROPIC_BASE_URL`.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Stop the proxy.
    pub fn shutdown(self) {
        self.task.abort();
    }
}

impl Drop for WiretapProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Built-in sink that appends exchanges to a file (one JSON object per line) or,
/// when no file is configured, logs them to the `chaos_clamp::wiretap` tracing
/// target. A background task owns the file so recording never blocks forwarding.
pub struct FileWiretapSink {
    tx: Option<mpsc::UnboundedSender<String>>,
}

impl FileWiretapSink {
    pub fn new(record_file: Option<PathBuf>) -> Self {
        let Some(path) = record_file else {
            return Self { tx: None };
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await;
            let mut file = match file {
                Ok(f) => f,
                Err(err) => {
                    warn!(path = %path.display(), "wiretap record file open failed: {err}");
                    return;
                }
            };
            while let Some(line) = rx.recv().await {
                if let Err(err) = file.write_all(line.as_bytes()).await {
                    warn!("wiretap record write failed: {err}");
                    break;
                }
                let _ = file.write_all(b"\n").await;
            }
        });
        Self { tx: Some(tx) }
    }
}

impl WiretapSink for FileWiretapSink {
    fn record(&self, exchange: WiretapExchange) {
        let line = exchange.to_json().to_string();
        match &self.tx {
            Some(tx) => {
                let _ = tx.send(line);
            }
            None => {
                debug!(target: "chaos_clamp::wiretap", "{line}");
            }
        }
    }
}

/// Request-side fields captured before the response body streams. Combined with
/// the response into a [`WiretapExchange`] once the stream completes.
struct RecordParts {
    method: String,
    path: String,
    headers: serde_json::Value,
    request: Option<serde_json::Value>,
}

impl RecordParts {
    fn into_exchange(
        self,
        status: Option<u16>,
        response: Option<String>,
        truncated: bool,
    ) -> WiretapExchange {
        WiretapExchange {
            method: self.method,
            path: self.path,
            headers: self.headers,
            request: self.request,
            status,
            response,
            response_truncated: truncated,
        }
    }
}

/// Rewrite a loopback request to target Anthropic over HTTPS, record it, and
/// stream the response back untouched — teeing the body into the sink as it
/// flows so the record captures both directions without buffering the stream.
async fn forward(
    req: Request,
    sink: Arc<dyn WiretapSink>,
    upstream: Upstream,
) -> Result<Response, std::convert::Infallible> {
    let (mut parts, body) = req.into_parts();

    let method = parts.method.to_string();
    let path = parts.uri.request_target().into_owned();

    let headers = redact_headers(&parts.headers);

    // Buffer the request body — it's small (prompt + tool schemas). The
    // response is the SSE stream and is never buffered here.
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => {
            warn!("wiretap: failed to read request body: {err}");
            return Ok(error_response(StatusCode::BAD_GATEWAY));
        }
    };
    let request = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
    let record = RecordParts {
        method,
        path,
        headers,
        request,
    };

    // Rewrite URI to the upstream scheme+authority, keeping path+query.
    parts.uri.set_scheme(upstream.scheme.clone());
    parts.uri.set_authority(upstream.authority.clone());
    parts.headers.insert(HOST, upstream.host_header());
    // Ask the upstream for identity encoding so the tee captures readable bytes;
    // the subprocess still receives a valid (uncompressed) response.
    parts.headers.remove("accept-encoding");

    let upstream_req = Request::from_parts(parts, Body::from(bytes));

    let tls = TlsClientConfig::new().with_alpn_http_auto();
    let client = EasyHttpWebClient::connector_builder()
        .with_default_transport_connector()
        .with_tls_proxy_support_using_rustls()
        .with_proxy_support()
        .with_tls_support_using_rustls_and_default_http_version(tls, Version::HTTP_11)
        .with_default_http_connector(Executor::default())
        .build_client();
    let client = (
        RemoveRequestHeaderLayer::hop_by_hop(),
        RemoveResponseHeaderLayer::hop_by_hop(),
        MapResponseBodyLayer::new_boxed_streaming_body(),
    )
        .into_layer(client);

    match client.serve(upstream_req).await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let (resp_parts, resp_body) = resp.into_parts();
            let tee = TeeBody::new(
                resp_body.into_data_stream(),
                record,
                status,
                sink,
                MAX_RESPONSE_CAPTURE,
            );
            Ok(Response::from_parts(resp_parts, Body::from_stream(tee)))
        }
        Err(err) => {
            warn!("wiretap: upstream error: {err:?}");
            sink.record(record.into_exchange(None, None, false));
            Ok(error_response(StatusCode::BAD_GATEWAY))
        }
    }
}

/// Streams the upstream response body to the client while accumulating a copy.
/// On stream completion (or drop) it records the full request+response envelope.
struct TeeBody {
    inner: BodyDataStream,
    buf: Vec<u8>,
    truncated: bool,
    cap: usize,
    /// Taken on flush; `None` once recorded so we never record twice.
    pending: Option<(RecordParts, u16)>,
    sink: Arc<dyn WiretapSink>,
}

impl TeeBody {
    fn new(
        inner: BodyDataStream,
        record: RecordParts,
        status: u16,
        sink: Arc<dyn WiretapSink>,
        cap: usize,
    ) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            truncated: false,
            cap,
            pending: Some((record, status)),
            sink,
        }
    }

    fn flush(&mut self) {
        if let Some((record, status)) = self.pending.take() {
            let body = String::from_utf8_lossy(&self.buf).into_owned();
            self.sink
                .record(record.into_exchange(Some(status), Some(body), self.truncated));
        }
    }
}

impl Stream for TeeBody {
    type Item = Result<Bytes, BoxError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                if !this.truncated {
                    if this.buf.len() + chunk.len() > this.cap {
                        this.truncated = true;
                    } else {
                        this.buf.extend_from_slice(&chunk);
                    }
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => {
                this.flush();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for TeeBody {
    fn drop(&mut self) {
        // Records the turn even if the client disconnects before EOF.
        self.flush();
    }
}

fn redact_headers(headers: &rama::http::HeaderMap) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (name, value) in headers {
        let key = name.as_str().to_ascii_lowercase();
        let val = if SENSITIVE_HEADERS.contains(&key.as_str()) {
            REDACTED.to_owned()
        } else {
            value.to_str().unwrap_or("<binary>").to_owned()
        };
        map.insert(key, serde_json::Value::String(val));
    }
    serde_json::Value::Object(map)
}

fn error_response(status: StatusCode) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = status;
    resp
}

#[cfg(test)]
mod tests {
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
            let resp = if req.uri().path() == "/error" {
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
                .with_tls_proxy_support_using_rustls()
                .with_proxy_support()
                .with_tls_support_using_rustls_and_default_http_version(tls, Version::HTTP_11)
                .with_default_http_connector(Executor::default())
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
    async fn proxy_records_non_json_request_body() {
        let upstream = start_mock_upstream().await;
        let sink = Arc::new(TestSink::default());
        let proxy = WiretapProxy::start_with_upstream(
            sink.clone(),
            &format!("http://127.0.0.1:{upstream}"),
        )
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
        let proxy = WiretapProxy::start_with_upstream(
            sink.clone(),
            &format!("http://127.0.0.1:{upstream}"),
        )
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
        let proxy = WiretapProxy::start_with_upstream(
            sink.clone(),
            &format!("http://127.0.0.1:{upstream}"),
        )
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
}
