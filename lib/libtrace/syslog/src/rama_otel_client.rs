//! OpenTelemetry HttpClient implementation backed by rama.

use rama::Service;
use rama::bytes::Bytes;
use rama::error::extra::OpaqueError;
use rama::http::Body;
use rama::http::body::util::BodyExt;
use rama::service::BoxService;
use std::fmt;
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
    fn send_bytes<'life0, 'async_trait>(
        &'life0 self,
        request: http::Request<Bytes>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<http::Response<Bytes>, HttpError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        let inner = self.inner.clone();
        Box::pin(async move {
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
