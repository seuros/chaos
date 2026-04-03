use crate::auth::AuthProvider;
use crate::auth::add_auth_headers;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::telemetry::run_with_request_telemetry;
use codex_client::HttpTransport;
use codex_client::Request;
use codex_client::RequestTelemetry;
use codex_client::Response;
use codex_client::StreamResponse;
use http::HeaderMap;
use http::Method;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::to_value;
use std::sync::Arc;
use tracing::instrument;

pub(crate) struct EndpointSession<T: HttpTransport, A: AuthProvider> {
    transport: T,
    provider: Provider,
    auth: A,
    request_telemetry: Option<Arc<dyn RequestTelemetry>>,
}

impl<T: HttpTransport, A: AuthProvider> EndpointSession<T, A> {
    pub(crate) fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            transport,
            provider,
            auth,
            request_telemetry: None,
        }
    }

    pub(crate) fn with_request_telemetry(
        mut self,
        request: Option<Arc<dyn RequestTelemetry>>,
    ) -> Self {
        self.request_telemetry = request;
        self
    }

    pub(crate) fn provider(&self) -> &Provider {
        &self.provider
    }

    fn make_request(
        &self,
        method: &Method,
        path: &str,
        extra_headers: &HeaderMap,
        body: Option<&Value>,
    ) -> Request {
        let mut req = self.provider.build_request(method.clone(), path);
        req.headers.extend(extra_headers.clone());
        if let Some(body) = body {
            req.body = Some(body.clone());
        }
        add_auth_headers(&self.auth, req)
    }

    pub(crate) async fn execute(
        &self,
        method: Method,
        path: &str,
        extra_headers: HeaderMap,
        body: Option<Value>,
    ) -> Result<Response, ApiError> {
        self.execute_with(method, path, extra_headers, body, |_| {})
            .await
    }

    #[instrument(
        name = "endpoint_session.execute_with",
        level = "info",
        skip_all,
        fields(http.method = %method, api.path = path)
    )]
    pub(crate) async fn execute_with<C>(
        &self,
        method: Method,
        path: &str,
        extra_headers: HeaderMap,
        body: Option<Value>,
        configure: C,
    ) -> Result<Response, ApiError>
    where
        C: Fn(&mut Request),
    {
        let make_request = || {
            let mut req = self.make_request(&method, path, &extra_headers, body.as_ref());
            configure(&mut req);
            req
        };

        let response = run_with_request_telemetry(
            self.provider.retry.to_policy(),
            self.request_telemetry.clone(),
            make_request,
            |req| self.transport.execute(req),
        )
        .await?;

        Ok(response)
    }

    pub(crate) async fn post_json_response<R>(
        &self,
        path: &str,
        extra_headers: HeaderMap,
        body: Value,
        decode_context: &str,
    ) -> Result<R, ApiError>
    where
        R: DeserializeOwned,
    {
        let response = self
            .execute(Method::POST, path, extra_headers, Some(body))
            .await?;
        self.decode_json_response(&response.body, decode_context)
    }

    pub(crate) async fn post_json_input<I, R>(
        &self,
        path: &str,
        input: &I,
        extra_headers: HeaderMap,
        encode_context: &str,
        decode_context: &str,
    ) -> Result<R, ApiError>
    where
        I: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let body = to_value(input)
            .map_err(|e| ApiError::Stream(format!("failed to encode {encode_context}: {e}")))?;
        self.post_json_response(path, extra_headers, body, decode_context)
            .await
    }

    pub(crate) fn decode_json_response<R>(
        &self,
        body: &[u8],
        decode_context: &str,
    ) -> Result<R, ApiError>
    where
        R: DeserializeOwned,
    {
        serde_json::from_slice(body)
            .map_err(|e| ApiError::Stream(format!("failed to decode {decode_context}: {e}")))
    }

    #[instrument(
        name = "endpoint_session.stream_with",
        level = "info",
        skip_all,
        fields(http.method = %method, api.path = path)
    )]
    pub(crate) async fn stream_with<C>(
        &self,
        method: Method,
        path: &str,
        extra_headers: HeaderMap,
        body: Option<Value>,
        configure: C,
    ) -> Result<StreamResponse, ApiError>
    where
        C: Fn(&mut Request),
    {
        let make_request = || {
            let mut req = self.make_request(&method, path, &extra_headers, body.as_ref());
            configure(&mut req);
            req
        };

        let stream = run_with_request_telemetry(
            self.provider.retry.to_policy(),
            self.request_telemetry.clone(),
            make_request,
            |req| self.transport.stream(req),
        )
        .await?;

        Ok(stream)
    }
}
