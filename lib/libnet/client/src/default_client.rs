use bytes::Bytes;
use rama::Service;
use rama::http::Body;
use rama::http::HeaderMap;
use rama::http::HeaderName;
use rama::http::HeaderValue;
use rama::http::HttpError;
use rama::http::Method;
use rama::http::body::util::BodyExt;
use serde::Serialize;
use std::fmt::Display;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::TransportError;
use crate::http_client::{RamaClient, raw_http_client, with_http_policies};
use crate::request::Response;
use crate::telemetry::inject_trace_headers;

/// HTTP client wrapper backed by rama. Provides convenience methods
/// (.get, .post, .send) with OpenTelemetry trace header injection.
#[derive(Clone)]
pub struct ChaosHttpClient {
    inner: Arc<Mutex<RamaClient>>,
    default_headers: HeaderMap,
}

impl std::fmt::Debug for ChaosHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChaosHttpClient").finish()
    }
}

impl ChaosHttpClient {
    pub fn new(client: RamaClient) -> Self {
        Self::new_with_egress(client, None)
    }

    pub fn new_with_egress(client: RamaClient, egress: Option<crate::Egress>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(with_http_policies(client, egress))),
            default_headers: HeaderMap::new(),
        }
    }

    pub fn default_client() -> Self {
        Self::default_client_with_egress(None)
    }

    pub fn default_client_with_egress(egress: Option<crate::Egress>) -> Self {
        Self::new_with_egress(raw_http_client(), egress)
    }

    pub fn get(&self, url: &str) -> ChaosRequestBuilder {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: &str) -> ChaosRequestBuilder {
        self.request(Method::POST, url)
    }

    pub fn request(&self, method: Method, url: &str) -> ChaosRequestBuilder {
        ChaosRequestBuilder {
            client: self.inner.clone(),
            method,
            url: url.to_string(),
            default_headers: self.default_headers.clone(),
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
        }
    }

    pub fn with_default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers = headers;
        self
    }
}

#[must_use = "requests are not sent unless `send` is awaited"]
pub struct ChaosRequestBuilder {
    client: Arc<Mutex<RamaClient>>,
    method: Method,
    url: String,
    default_headers: HeaderMap,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    timeout: Option<Duration>,
}

impl std::fmt::Debug for ChaosRequestBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChaosRequestBuilder")
            .field("method", &self.method)
            .field("url", &self.url)
            .finish()
    }
}

impl ChaosRequestBuilder {
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

    /// Deadline from `send`, including the client lock and response body.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
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

    pub async fn send(self) -> Result<ChaosResponse, ChaosClientError> {
        let deadline = self
            .timeout
            .and_then(|timeout| Instant::now().checked_add(timeout));
        let mut headers = self.default_headers;
        for (key, value) in &self.headers {
            headers.insert(key, value.clone());
        }

        inject_trace_headers(&mut headers);

        let rama_body = match self.body {
            Some(bytes) => Body::from(bytes),
            None => Body::empty(),
        };

        let mut builder = rama::http::Request::builder()
            .method(self.method.clone())
            .uri(self.url.as_str());

        for (key, value) in headers.iter() {
            builder = builder.header(key, value);
        }

        let request = builder
            .body(rama_body)
            .map_err(|e| ChaosClientError::Build(e.to_string()))?;

        let response = within_deadline(deadline, async {
            self.client
                .lock()
                .await
                .serve(request)
                .await
                .map_err(|e| ChaosClientError::Network(e.to_string()))
        })
        .await?;

        tracing::debug!(
            method = %self.method,
            url = %self.url,
            status = %response.status(),
            "Request completed"
        );

        Ok(ChaosResponse {
            inner: response,
            deadline,
        })
    }

    /// Buffer the response; non-success statuses become `TransportError::Http`.
    pub async fn execute(self) -> Result<Response, TransportError> {
        let url = self.url.clone();
        let response = self.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await?;
        Response {
            status,
            headers,
            body,
        }
        .into_result(url)
    }
}

/// Response wrapper providing convenience methods over rama's Response.
pub struct ChaosResponse {
    inner: rama::http::Response,
    deadline: Option<Instant>,
}

impl ChaosResponse {
    pub fn status(&self) -> rama::http::StatusCode {
        self.inner.status()
    }

    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    pub async fn bytes(self) -> Result<Bytes, ChaosClientError> {
        within_deadline(self.deadline, async {
            self.inner
                .into_body()
                .collect()
                .await
                .map(rama::http::body::util::Collected::to_bytes)
                .map_err(|e| ChaosClientError::Body(e.to_string()))
        })
        .await
    }

    pub async fn text(self) -> Result<String, ChaosClientError> {
        let bytes = self.bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, ChaosClientError> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| ChaosClientError::Json(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChaosClientError {
    #[error("request timed out")]
    Timeout,
    #[error("request build error: {0}")]
    Build(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("body read error: {0}")]
    Body(String),
    #[error("json error: {0}")]
    Json(String),
}

impl From<ChaosClientError> for TransportError {
    fn from(error: ChaosClientError) -> Self {
        match error {
            ChaosClientError::Timeout => Self::Timeout,
            ChaosClientError::Build(message) | ChaosClientError::Json(message) => {
                Self::Build(message)
            }
            ChaosClientError::Network(message) | ChaosClientError::Body(message) => {
                Self::Network(message)
            }
        }
    }
}

async fn within_deadline<T>(
    deadline: Option<Instant>,
    future: impl Future<Output = Result<T, ChaosClientError>>,
) -> Result<T, ChaosClientError> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, future)
            .await
            .map_err(|_| ChaosClientError::Timeout)?,
        None => future.await,
    }
}

#[cfg(test)]
mod tests;
