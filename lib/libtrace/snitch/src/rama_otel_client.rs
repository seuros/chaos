//! OpenTelemetry HttpClient implementation backed by rama.

use rama::Service;
use rama::bytes::Bytes;
use rama::error::extra::OpaqueError;
use rama::http::Body;
use rama::http::body::util::BodyExt;
use rama::service::BoxService;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::runtime::Handle;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

type HttpError = Box<dyn std::error::Error + Send + Sync>;

/// Background tokio runtime used when the OTLP exporter drives the http
/// client from a thread that has no current tokio context. The OpenTelemetry
/// SDK's non-tokio batch processors and current-thread test runtimes cannot
/// drive rama's async stack themselves, so a dedicated multi-threaded
/// runtime is shared across all such requests.
fn fallback_runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("chaos-otel-http")
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => panic!("rama otel http runtime should build: {err}"),
        }
    })
}

/// Async rama-based HTTP client for OpenTelemetry OTLP exporters.
///
/// Implements opentelemetry_http::HttpClient so it can be passed to
/// the OTLP exporter's `.with_http_client()`.
pub(crate) struct RamaOtelClient {
    inner: Arc<Mutex<BoxService<rama::http::Request, rama::http::Response, OpaqueError>>>,
}

impl RamaOtelClient {
    pub fn new() -> Self {
        let client = rama::http::client::EasyHttpWebClient::default().boxed();
        Self {
            inner: Arc::new(Mutex::new(client)),
        }
    }
}

impl fmt::Debug for RamaOtelClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RamaOtelClient").finish()
    }
}

async fn drive_request(
    inner: Arc<Mutex<BoxService<rama::http::Request, rama::http::Response, OpaqueError>>>,
    request: http::Request<Bytes>,
) -> Result<http::Response<Bytes>, HttpError> {
    let (parts, body) = request.into_parts();
    let rama_body = Body::from(body.to_vec());
    let rama_request = rama::http::Request::from_parts(parts.into(), rama_body);

    let rama_response: rama::http::Response = inner
        .lock()
        .await
        .serve(rama_request)
        .await
        .map_err(|e| -> HttpError { Box::new(std::io::Error::other(e.to_string())) })?;

    let (parts, body) = rama_response.into_parts();
    let collected = BodyExt::collect(body)
        .await
        .map_err(|e| -> HttpError { Box::new(std::io::Error::other(e.to_string())) })?;
    let body_bytes = collected.to_bytes();

    Ok(http::Response::from_parts(parts.into(), body_bytes))
}

impl opentelemetry_http::HttpClient for RamaOtelClient {
    fn send_bytes<'life0, 'otel>(
        &'life0 self,
        request: http::Request<Bytes>,
    ) -> Pin<Box<dyn Future<Output = Result<http::Response<Bytes>, HttpError>> + Send + 'otel>>
    where
        'life0: 'otel,
        Self: 'otel,
    {
        let inner = self.inner.clone();
        Box::pin(async move {
            // The OTLP exporter may drive this future from a thread that has
            // no current tokio runtime — for example the non-tokio
            // BatchSpanProcessor or a synchronous PeriodicReader on a plain
            // `#[test]`. Spawn the request onto a long-lived background
            // runtime in that case so rama's async transport always has the
            // reactor it needs.
            if Handle::try_current().is_ok() {
                drive_request(inner, request).await
            } else {
                let runtime_handle = fallback_runtime().handle().clone();
                runtime_handle
                    .spawn(drive_request(inner, request))
                    .await
                    .map_err(|e| -> HttpError { Box::new(std::io::Error::other(e.to_string())) })?
            }
        })
    }
}
