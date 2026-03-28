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
use tokio::sync::Mutex;

type HttpError = Box<dyn std::error::Error + Send + Sync>;

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

impl opentelemetry_http::HttpClient for RamaOtelClient {
    fn send_bytes<'life0, 'otel>(
        &'life0 self,
        request: http::Request<Bytes>,
    ) -> Pin<Box<dyn Future<Output = Result<http::Response<Bytes>, HttpError>> + Send + 'otel>>
    where
        'life0: 'otel,
        Self: 'otel,
    {
        Box::pin(async move {
            let inner = self.inner.clone();
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
        })
    }
}
