use bytes::Bytes;
use opentelemetry::global;
use opentelemetry::propagation::Injector;
use rama::Service;
use rama::error::extra::OpaqueError;
use rama::http::Body;
use rama::http::HeaderMap;
use rama::http::HeaderName;
use rama::http::HeaderValue;
use rama::http::HttpError;
use rama::http::Method;
use rama::http::body::util::BodyExt;
use rama::service::BoxService;
use serde::Serialize;
use std::fmt::Display;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::ensure_rustls_crypto_provider;

type RamaClient = BoxService<rama::http::Request, rama::http::Response, OpaqueError>;

/// HTTP client wrapper backed by rama. Provides convenience methods
/// (.get, .post, .send) with OpenTelemetry trace header injection.
#[derive(Clone)]
pub struct CodexHttpClient {
    inner: Arc<Mutex<RamaClient>>,
    default_headers: HeaderMap,
}

impl std::fmt::Debug for CodexHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexHttpClient").finish()
    }
}

impl CodexHttpClient {
    pub fn new(client: RamaClient) -> Self {
        Self {
            inner: Arc::new(Mutex::new(client)),
            default_headers: HeaderMap::new(),
        }
    }

    pub fn default_client() -> Self {
        use rama::Service;
        ensure_rustls_crypto_provider();
        Self::new(rama::http::client::EasyHttpWebClient::default().boxed())
    }

    pub fn get(&self, url: &str) -> CodexRequestBuilder {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: &str) -> CodexRequestBuilder {
        self.request(Method::POST, url)
    }

    pub fn request(&self, method: Method, url: &str) -> CodexRequestBuilder {
        CodexRequestBuilder {
            client: self.inner.clone(),
            method,
            url: url.to_string(),
            default_headers: self.default_headers.clone(),
            headers: HeaderMap::new(),
            body: None,
        }
    }

    pub fn with_default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers = headers;
        self
    }
}

#[must_use = "requests are not sent unless `send` is awaited"]
pub struct CodexRequestBuilder {
    client: Arc<Mutex<RamaClient>>,
    method: Method,
    url: String,
    default_headers: HeaderMap,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
}

impl std::fmt::Debug for CodexRequestBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexRequestBuilder")
            .field("method", &self.method)
            .field("url", &self.url)
            .finish()
    }
}

impl CodexRequestBuilder {
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        HeaderName: TryFrom<K>,
        <HeaderName as TryFrom<K>>::Error: Into<HttpError>,
        HeaderValue: TryFrom<V>,
        <HeaderValue as TryFrom<V>>::Error: Into<HttpError>,
    {
        if let (Ok(name), Ok(val)) = (HeaderName::try_from(key), HeaderValue::try_from(value)) {
            self.headers.insert(name, val);
        }
        self
    }

    pub fn bearer_auth<T>(self, token: T) -> Self
    where
        T: Display,
    {
        self.header(rama::http::header::AUTHORIZATION, format!("Bearer {token}"))
    }

    pub fn timeout(self, _timeout: std::time::Duration) -> Self {
        // TODO: implement per-request timeout via rama layer
        self
    }

    pub fn json<T>(mut self, value: &T) -> Self
    where
        T: ?Sized + Serialize,
    {
        if let Ok(bytes) = serde_json::to_vec(value) {
            self.body = Some(bytes);
            self.headers.insert(
                rama::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }
        self
    }

    pub fn body<B: Into<Vec<u8>>>(mut self, body: B) -> Self {
        self.body = Some(body.into());
        self
    }

    pub async fn send(self) -> Result<CodexResponse, CodexClientError> {
        let mut headers = self.default_headers;
        for (key, value) in &self.headers {
            headers.insert(key, value.clone());
        }

        // Inject trace headers.
        inject_trace_headers(&mut headers);

        let rama_body = match self.body {
            Some(bytes) => Body::from(bytes),
            None => Body::empty(),
        };

        let mut builder = rama::http::Request::builder()
            .method(self.method.clone())
            .uri(&self.url);

        for (key, value) in headers.iter() {
            builder = builder.header(key, value);
        }

        let request = builder
            .body(rama_body)
            .map_err(|e| CodexClientError::Build(e.to_string()))?;

        let response = self
            .client
            .lock()
            .await
            .serve(request)
            .await
            .map_err(|e| CodexClientError::Network(e.to_string()))?;

        tracing::debug!(
            method = %self.method,
            url = %self.url,
            status = %response.status(),
            "Request completed"
        );

        Ok(CodexResponse { inner: response })
    }
}

/// Response wrapper providing convenience methods over rama's Response.
pub struct CodexResponse {
    inner: rama::http::Response,
}

impl CodexResponse {
    pub fn status(&self) -> rama::http::StatusCode {
        self.inner.status()
    }

    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    pub async fn bytes(self) -> Result<Bytes, CodexClientError> {
        self.inner
            .into_body()
            .collect()
            .await
            .map(rama::http::body::util::Collected::to_bytes)
            .map_err(|e| CodexClientError::Body(e.to_string()))
    }

    pub async fn text(self) -> Result<String, CodexClientError> {
        let bytes = self.bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, CodexClientError> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| CodexClientError::Json(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodexClientError {
    #[error("request build error: {0}")]
    Build(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("body read error: {0}")]
    Body(String),
    #[error("json error: {0}")]
    Json(String),
}

fn inject_trace_headers(headers: &mut HeaderMap) {
    struct HeaderMapInjector<'a>(&'a mut HeaderMap);

    impl Injector for HeaderMapInjector<'_> {
        fn set(&mut self, key: &str, value: String) {
            if let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                self.0.insert(name, val);
            }
        }
    }

    global::get_text_map_propagator(|prop| {
        prop.inject_context(&Span::current().context(), &mut HeaderMapInjector(headers));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::propagation::Extractor;
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing::trace_span;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    #[test]
    fn inject_trace_headers_uses_current_span_context() {
        global::set_text_map_propagator(TraceContextPropagator::new());

        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test-tracer");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        let _guard = subscriber.set_default();

        let span = trace_span!("client_request");
        let _entered = span.enter();
        let span_context = span.context().span().span_context().clone();

        let mut headers = HeaderMap::new();
        inject_trace_headers(&mut headers);

        let extractor = HeaderMapExtractor(&headers);
        let extracted = TraceContextPropagator::new().extract(&extractor);
        let extracted_span = extracted.span();
        let extracted_context = extracted_span.span_context();

        assert!(extracted_context.is_valid());
        assert_eq!(extracted_context.trace_id(), span_context.trace_id());
        assert_eq!(extracted_context.span_id(), span_context.span_id());
    }

    struct HeaderMapExtractor<'a>(&'a HeaderMap);

    impl Extractor for HeaderMapExtractor<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0.get(key).and_then(|value| value.to_str().ok())
        }

        fn keys(&self) -> Vec<&str> {
            self.0.keys().map(HeaderName::as_str).collect()
        }
    }
}
