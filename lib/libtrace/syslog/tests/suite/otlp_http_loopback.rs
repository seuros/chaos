use chaos_ipc::product::CHAOS_VERSION;
use chaos_syslog::OtelProvider;
use chaos_syslog::config::OtelExporter;
use chaos_syslog::config::OtelHttpProtocol;
use chaos_syslog::config::OtelSettings;
use chaos_syslog::metrics::MetricsClient;
use chaos_syslog::metrics::MetricsConfig;
use std::any::Any;
use std::collections::HashMap;
use std::io::Read as _;
use std::io::Write as _;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tracing_subscriber::layer::SubscriberExt;

struct CapturedRequest {
    path: String,
    content_type: Option<String>,
    body: Vec<u8>,
}

struct LoopbackCollector {
    addr: SocketAddr,
    rx: mpsc::Receiver<Vec<CapturedRequest>>,
    server: thread::JoinHandle<()>,
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".to_string())
}

impl LoopbackCollector {
    fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;

        let (tx, rx) = mpsc::channel::<Vec<CapturedRequest>>();
        let server = thread::spawn(move || {
            let mut captured = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(3);

            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Some(request) = capture_request(&mut stream) {
                            captured.push(request);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }

            let _ = tx.send(captured);
        });

        Ok(Self { addr, rx, server })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn finish(self) -> std::io::Result<Vec<CapturedRequest>> {
        self.server.join().map_err(|payload| {
            std::io::Error::other(format!(
                "server thread panicked: {}",
                panic_payload_message(&*payload)
            ))
        })?;
        self.rx.recv_timeout(Duration::from_secs(1)).map_err(|err| {
            std::io::Error::other(format!("failed to receive captured requests: {err}"))
        })
    }
}

fn read_http_request(
    stream: &mut TcpStream,
) -> std::io::Result<(String, HashMap<String, String>, Vec<u8>)> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let deadline = Instant::now() + Duration::from_secs(2);

    let mut read_next = |buf: &mut [u8]| -> std::io::Result<usize> {
        loop {
            match stream.read(buf) {
                Ok(n) => return Ok(n),
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::Interrupted =>
                {
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "timed out waiting for request data",
                        ));
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(err) => return Err(err),
            }
        }
    };

    let mut buf = Vec::new();
    let mut scratch = [0u8; 8192];
    let header_end = loop {
        let n = read_next(&mut scratch)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "EOF before headers",
            ));
        }
        buf.extend_from_slice(&scratch[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        if buf.len() > 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "headers too large",
            ));
        }
    };

    let headers_bytes = &buf[..header_end];
    let mut body_bytes = buf[header_end + 4..].to_vec();

    let headers_str = std::str::from_utf8(headers_bytes).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("headers not utf-8: {err}"),
        )
    })?;
    let mut lines = headers_str.split("\r\n");
    let start = lines.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing request line")
    })?;
    let mut parts = start.split_whitespace();
    let _method = parts.next().unwrap_or_default();
    let path = parts
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing path"))?
        .to_string();

    let mut headers = HashMap::new();
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }

    if let Some(len) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        while body_bytes.len() < len {
            let n = read_next(&mut scratch)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "EOF before body complete",
                ));
            }
            body_bytes.extend_from_slice(&scratch[..n]);
            if body_bytes.len() > len + 1024 * 1024 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "body too large",
                ));
            }
        }
        body_bytes.truncate(len);
    }

    Ok((path, headers, body_bytes))
}

fn write_http_response(stream: &mut TcpStream, status: &str) -> std::io::Result<()> {
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn capture_request(stream: &mut TcpStream) -> Option<CapturedRequest> {
    let result = read_http_request(stream);
    let _ = write_http_response(stream, "202 Accepted");
    let (path, headers, body) = result.ok()?;

    Some(CapturedRequest {
        path,
        content_type: headers.get("content-type").cloned(),
        body,
    })
}

fn captured_request<'a>(captured: &'a [CapturedRequest], path: &str) -> &'a CapturedRequest {
    captured
        .iter()
        .find(|req| req.path == path)
        .unwrap_or_else(|| {
            let paths = captured
                .iter()
                .map(|req| req.path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            panic!("missing {path} request; got {}: {paths}", captured.len());
        })
}

fn assert_json_request_contains(
    captured: &[CapturedRequest],
    path: &str,
    expected_fragments: &[(&str, &str)],
) {
    let request = captured_request(captured, path);
    let content_type = request
        .content_type
        .as_deref()
        .unwrap_or("<missing content-type>");
    assert!(
        content_type.starts_with("application/json"),
        "unexpected content-type: {content_type}"
    );

    let body = String::from_utf8_lossy(&request.body);
    let body_prefix = body.chars().take(2000).collect::<String>();
    for &(needle, description) in expected_fragments {
        assert!(
            body.contains(needle),
            "expected {description} not found; body prefix: {body_prefix}"
        );
    }
}

fn trace_settings(endpoint: String) -> OtelSettings {
    OtelSettings {
        environment: "test".to_string(),
        service_name: "chaos-cli".to_string(),
        service_version: CHAOS_VERSION.to_string(),
        chaos_home: PathBuf::from("."),
        exporter: OtelExporter::None,
        trace_exporter: OtelExporter::OtlpHttp {
            endpoint,
            headers: HashMap::new(),
            protocol: OtelHttpProtocol::Json,
            tls: None,
        },
        metrics_exporter: OtelExporter::None,
        runtime_metrics: false,
    }
}

#[test]
fn otlp_http_exporter_sends_metrics_to_collector()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let collector = LoopbackCollector::bind()?;

    let metrics = MetricsClient::new(MetricsConfig::otlp(
        "test",
        "chaos-cli",
        CHAOS_VERSION,
        OtelExporter::OtlpHttp {
            endpoint: collector.endpoint("/v1/metrics"),
            headers: HashMap::new(),
            protocol: OtelHttpProtocol::Json,
            tls: None,
        },
    ))?;

    metrics.counter("chaos.turns", 1, &[("source", "test")])?;
    metrics.shutdown()?;

    let captured = collector.finish()?;
    assert_json_request_contains(&captured, "/v1/metrics", &[("chaos.turns", "metric name")]);

    Ok(())
}

#[test]
fn otlp_http_exporter_sends_traces_to_collector()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let collector = LoopbackCollector::bind()?;

    let otel = OtelProvider::from(&trace_settings(collector.endpoint("/v1/traces")))?
        .ok_or_else(|| std::io::Error::other("expected otel provider"))?;
    let tracing_layer = otel
        .tracing_layer()
        .ok_or_else(|| std::io::Error::other("expected tracing layer"))?;
    let subscriber = tracing_subscriber::registry().with(tracing_layer);

    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!(
            "trace-loopback",
            otel.name = "trace-loopback",
            otel.kind = "server",
            rpc.system = "jsonrpc",
            rpc.method = "trace-loopback",
        );
        let _guard = span.enter();
        tracing::info!("trace loopback event");
    });
    otel.shutdown();

    let captured = collector.finish()?;
    assert_json_request_contains(
        &captured,
        "/v1/traces",
        &[
            ("trace-loopback", "span name"),
            ("chaos-cli", "service name"),
        ],
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_http_exporter_sends_traces_to_collector_in_tokio_runtime()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let collector = LoopbackCollector::bind()?;

    let otel = OtelProvider::from(&trace_settings(collector.endpoint("/v1/traces")))?
        .ok_or_else(|| std::io::Error::other("expected otel provider"))?;
    let tracing_layer = otel
        .tracing_layer()
        .ok_or_else(|| std::io::Error::other("expected tracing layer"))?;
    let subscriber = tracing_subscriber::registry().with(tracing_layer);

    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!(
            "trace-loopback-tokio",
            otel.name = "trace-loopback-tokio",
            otel.kind = "server",
            rpc.system = "jsonrpc",
            rpc.method = "trace-loopback-tokio",
        );
        let _guard = span.enter();
        tracing::info!("trace loopback event from tokio runtime");
    });
    otel.shutdown();

    let captured = collector.finish()?;
    assert_json_request_contains(
        &captured,
        "/v1/traces",
        &[
            ("trace-loopback-tokio", "span name"),
            ("chaos-cli", "service name"),
        ],
    );

    Ok(())
}

#[test]
fn otlp_http_exporter_sends_traces_to_collector_in_current_thread_tokio_runtime()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let collector = LoopbackCollector::bind()?;
    let addr = collector.addr;

    let (runtime_result_tx, runtime_result_rx) = mpsc::channel::<std::result::Result<(), String>>();
    let runtime_thread = thread::spawn(move || {
        let result = (|| -> std::result::Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| err.to_string())?;

            runtime.block_on(async move {
                let otel = OtelProvider::from(&trace_settings(format!("http://{addr}/v1/traces")))
                    .map_err(|err| err.to_string())?
                    .ok_or_else(|| "expected otel provider".to_string())?;
                let tracing_layer = otel
                    .tracing_layer()
                    .ok_or_else(|| "expected tracing layer".to_string())?;
                let subscriber = tracing_subscriber::registry().with(tracing_layer);

                tracing::subscriber::with_default(subscriber, || {
                    let span = tracing::info_span!(
                        "trace-loopback-current-thread",
                        otel.name = "trace-loopback-current-thread",
                        otel.kind = "server",
                        rpc.system = "jsonrpc",
                        rpc.method = "trace-loopback-current-thread",
                    );
                    let _guard = span.enter();
                    tracing::info!("trace loopback event from current-thread tokio runtime");
                });
                otel.shutdown();
                Ok::<(), String>(())
            })
        })();
        let _ = runtime_result_tx.send(result);
    });

    runtime_result_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|err| {
            std::io::Error::other(format!("current-thread runtime should complete: {err}"))
        })?
        .map_err(std::io::Error::other)?;
    runtime_thread.join().map_err(|payload| {
        std::io::Error::other(format!(
            "runtime thread panicked: {}",
            panic_payload_message(&*payload)
        ))
    })?;

    let captured = collector.finish()?;
    assert_json_request_contains(
        &captured,
        "/v1/traces",
        &[
            ("trace-loopback-current-thread", "span name"),
            ("chaos-cli", "service name"),
        ],
    );

    Ok(())
}
