use crate::error::TransportError;
use crate::request::Request;
use crate::request::RequestCompression;
use crate::request::Response;
use bytes::Bytes;
use futures::stream::BoxStream;
use rama::http::HeaderMap;
use rama::http::StatusCode;
use rama::Service;
use rama::error::extra::OpaqueError;
use rama::http::Body;
use rama::http::body::util::BodyExt;
use rama::service::BoxService;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::Level;
use tracing::enabled;
use tracing::trace;

use crate::ensure_rustls_crypto_provider;

pub type ByteStream = BoxStream<'static, Result<Bytes, TransportError>>;

pub struct StreamResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub bytes: ByteStream,
}

#[allow(async_fn_in_trait)]
pub trait HttpTransport: Send + Sync {
    async fn execute(&self, req: Request) -> Result<Response, TransportError>;
    async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError>;
}

type RamaClient = BoxService<rama::http::Request, rama::http::Response, OpaqueError>;

#[derive(Clone)]
pub struct RamaTransport {
    client: Arc<Mutex<RamaClient>>,
}

impl RamaTransport {
    pub fn new(client: RamaClient) -> Self {
        Self {
            client: Arc::new(Mutex::new(client)),
        }
    }

    pub fn default_client() -> Self {
        use rama::Service;
        ensure_rustls_crypto_provider();
        Self::new(rama::http::client::EasyHttpWebClient::default().boxed())
    }

    fn build_request(req: Request) -> Result<rama::http::Request, TransportError> {
        let Request {
            method,
            url,
            mut headers,
            body,
            compression,
            timeout: _timeout, // TODO: rama per-request timeout via layer
        } = req;

        let http_method =
            rama::http::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(rama::http::Method::GET);

        let rama_body = if let Some(body) = body {
            if compression != RequestCompression::None {
                if headers.contains_key(rama::http::header::CONTENT_ENCODING) {
                    return Err(TransportError::Build(
                        "request compression was requested but content-encoding is already set"
                            .to_string(),
                    ));
                }

                let json = serde_json::to_vec(&body)
                    .map_err(|err| TransportError::Build(err.to_string()))?;
                let pre_compression_bytes = json.len();
                let compression_start = std::time::Instant::now();
                let (compressed, content_encoding) = match compression {
                    RequestCompression::None => unreachable!("guarded by compression != None"),
                    RequestCompression::Zstd => (
                        zstd::stream::encode_all(std::io::Cursor::new(json), 3)
                            .map_err(|err| TransportError::Build(err.to_string()))?,
                        rama::http::HeaderValue::from_static("zstd"),
                    ),
                };
                let post_compression_bytes = compressed.len();
                let compression_duration = compression_start.elapsed();

                headers.insert(rama::http::header::CONTENT_ENCODING, content_encoding);
                if !headers.contains_key(rama::http::header::CONTENT_TYPE) {
                    headers.insert(
                        rama::http::header::CONTENT_TYPE,
                        rama::http::HeaderValue::from_static("application/json"),
                    );
                }

                tracing::info!(
                    pre_compression_bytes,
                    post_compression_bytes,
                    compression_duration_ms = compression_duration.as_millis(),
                    "Compressed request body with zstd"
                );

                Body::from(compressed)
            } else {
                if !headers.contains_key(rama::http::header::CONTENT_TYPE) {
                    headers.insert(
                        rama::http::header::CONTENT_TYPE,
                        rama::http::HeaderValue::from_static("application/json"),
                    );
                }
                let json_bytes = serde_json::to_vec(&body)
                    .map_err(|err| TransportError::Build(err.to_string()))?;
                Body::from(json_bytes)
            }
        } else {
            Body::empty()
        };

        // Inject trace headers.
        inject_trace_headers(&mut headers);

        let mut builder = rama::http::Request::builder().method(http_method).uri(&url);

        for (key, value) in headers.iter() {
            builder = builder.header(key, value);
        }

        builder
            .body(rama_body)
            .map_err(|err| TransportError::Build(err.to_string()))
    }
}

fn inject_trace_headers(headers: &mut HeaderMap) {
    use opentelemetry::global;
    use opentelemetry::propagation::Injector;
    use tracing::Span;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    struct HeaderMapInjector<'a>(&'a mut HeaderMap);

    impl Injector for HeaderMapInjector<'_> {
        fn set(&mut self, key: &str, value: String) {
            if let (Ok(name), Ok(val)) = (
                rama::http::HeaderName::from_bytes(key.as_bytes()),
                rama::http::HeaderValue::from_str(&value),
            ) {
                self.0.insert(name, val);
            }
        }
    }

    global::get_text_map_propagator(|prop| {
        prop.inject_context(&Span::current().context(), &mut HeaderMapInjector(headers));
    });
}

impl HttpTransport for RamaTransport {
    async fn execute(&self, req: Request) -> Result<Response, TransportError> {
        if enabled!(Level::TRACE) {
            trace!(
                "{} to {}: {}",
                req.method,
                req.url,
                req.body.as_ref().unwrap_or_default()
            );
        }

        let url = req.url.clone();
        let request = Self::build_request(req)?;
        let response = self
            .client
            .lock()
            .await
            .serve(request)
            .await
            .map_err(|err| TransportError::Network(err.to_string()))?;

        let status = response.status();
        let headers = response.headers().clone();
        let body_bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|err| TransportError::Network(err.to_string()))?
            .to_bytes();

        if !status.is_success() {
            let body = String::from_utf8(body_bytes.to_vec()).ok();
            return Err(TransportError::Http {
                status,
                url: Some(url),
                headers: Some(headers),
                body,
            });
        }

        Ok(Response {
            status,
            headers,
            body: body_bytes,
        })
    }

    async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError> {
        if enabled!(Level::TRACE) {
            trace!(
                "{} to {}: {}",
                req.method,
                req.url,
                req.body.as_ref().unwrap_or_default()
            );
        }

        let url = req.url.clone();
        let request = Self::build_request(req)?;
        let response = self
            .client
            .lock()
            .await
            .serve(request)
            .await
            .map_err(|err| TransportError::Network(err.to_string()))?;

        let status = response.status();
        let headers = response.headers().clone();

        if !status.is_success() {
            let body_bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|err| TransportError::Network(err.to_string()))?
                .to_bytes();
            let body = String::from_utf8(body_bytes.to_vec()).ok();
            return Err(TransportError::Http {
                status,
                url: Some(url),
                headers: Some(headers),
                body,
            });
        }

        let body_stream = response.into_body().into_data_stream();
        let stream = tokio_stream::StreamExt::map(body_stream, |result: Result<Bytes, _>| {
            result.map_err(|err| TransportError::Network(err.to_string()))
        });

        Ok(StreamResponse {
            status,
            headers,
            bytes: Box::pin(stream),
        })
    }
}
