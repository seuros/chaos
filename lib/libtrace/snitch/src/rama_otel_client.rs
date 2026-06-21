//! OpenTelemetry HttpClient implementation backed by rama.

use rama::Service;
use rama::bytes::Bytes;
use rama::error::extra::OpaqueError;
use rama::http::Body;
use rama::http::body::util::BodyExt;
use rama::service::BoxService;
use rama_http_hyperium::{TryIntoHyperiumHttp, TryIntoRamaHttp};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::runtime::Handle;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

type HttpError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
enum RamaOtelClientError {
    #[error("failed to convert OpenTelemetry HTTP request into rama HTTP request")]
    RequestConversion(#[source] rama::http::HttpError),

    #[error("rama OpenTelemetry HTTP request failed")]
    Request(#[source] OpaqueError),

    #[error("failed to collect rama OpenTelemetry HTTP response body")]
    ResponseBody(#[source] rama::error::BoxError),

    #[error("failed to convert rama HTTP response into OpenTelemetry HTTP response")]
    ResponseConversion(#[source] http::Error),

    #[error("rama OpenTelemetry HTTP runtime task failed")]
    RuntimeJoin(#[source] tokio::task::JoinError),
}

impl RamaOtelClientError {
    fn boxed(self) -> HttpError {
        Box::new(self)
    }
}

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
    let rama_request = request
        .map(Body::from)
        .try_into_rama_http()
        .map_err(|err| RamaOtelClientError::RequestConversion(err).boxed())?;

    let rama_response: rama::http::Response = inner
        .lock()
        .await
        .serve(rama_request)
        .await
        .map_err(|err| RamaOtelClientError::Request(err).boxed())?;

    let (parts, body) = rama_response.into_parts();
    let collected = BodyExt::collect(body)
        .await
        .map_err(|err| RamaOtelClientError::ResponseBody(err).boxed())?;
    let body_bytes = collected.to_bytes();

    rama::http::Response::from_parts(parts, body_bytes)
        .try_into_hyperium_http()
        .map_err(|err| RamaOtelClientError::ResponseConversion(err).boxed())
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
                    .map_err(|err| RamaOtelClientError::RuntimeJoin(err).boxed())?
            }
        })
    }
}
