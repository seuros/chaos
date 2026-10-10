use crate::auth::AuthProvider;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::headers::build_conversation_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use crate::requests::responses::Compression;
use crate::requests::responses::attach_item_ids;
use crate::sse::spawn_response_stream;
use crate::telemetry::SseTelemetry;
use chaos_client::HttpTransport;
use chaos_client::RequestCompression;
use chaos_client::RequestTelemetry;
use chaos_ipc::protocol::SessionSource;
use rama::bytes::Bytes;
use rama::http::HeaderMap;
use rama::http::HeaderValue;
use rama::http::Method;
use serde_json::Value;
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::debug;
use tracing::instrument;

mod websocket;
pub use websocket::ResponsesWebSocket;

pub struct ResponsesClient<T: HttpTransport, A: AuthProvider> {
    session: EndpointSession<T, A>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
    websocket: Option<Arc<ResponsesWebSocket>>,
}

#[derive(Default, Clone)]
pub struct ResponsesOptions {
    pub conversation_id: Option<String>,
    pub session_source: Option<SessionSource>,
    pub extra_headers: HeaderMap,
    pub compression: Compression,
    pub turn_state: Option<Arc<OnceLock<String>>>,
    /// Some endpoints authorize hosted search on the socket's first request.
    /// Reconnect before adding search to a socket initialized without it.
    pub websocket_reconnect_on_web_search_activation: bool,
}

impl<T: HttpTransport, A: AuthProvider> ResponsesClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
            websocket: None,
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
            websocket: self.websocket,
        }
    }

    pub fn with_websocket(mut self, websocket: Arc<ResponsesWebSocket>) -> Self {
        self.websocket = Some(websocket);
        self
    }

    fn connection(&self, headers: &HeaderMap) -> Option<websocket::Connection> {
        let provider = self.session.provider();
        // Gateway/DLP policies require an inspectable HTTP JSON body. Never
        // bypass configured egress with a direct websocket connection.
        if provider.egress.is_some() || provider.is_azure_responses_endpoint() {
            return None;
        }
        let request = self
            .session
            .make_request(&Method::GET, Self::path(), headers, None);
        Some(websocket::Connection {
            url: request.url,
            headers: request.headers,
            idle_timeout: jiff::SignedDuration::try_from(provider.stream_idle_timeout)
                .unwrap_or(jiff::SignedDuration::MAX),
            telemetry: self.session.request_telemetry(),
        })
    }

    pub fn prewarm(&self, options: &ResponsesOptions) {
        if let Some(websocket) = &self.websocket
            && let Some(connection) = self.connection(&responses_headers(options))
        {
            websocket.prewarm(connection);
        }
    }

    #[instrument(
        name = "responses.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = tracing::field::Empty,
            api.path = "responses"
        )
    )]
    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
        options: ResponsesOptions,
    ) -> Result<ResponseStream, ApiError> {
        let headers = responses_headers(&options);
        if let Some(websocket) = &self.websocket
            && let Some(connection) = self.connection(&headers)
        {
            tracing::Span::current().record("transport", "responses_websocket");
            return websocket
                .stream(
                    &request,
                    connection,
                    options.turn_state.clone(),
                    options.websocket_reconnect_on_web_search_activation,
                )
                .await;
        }
        tracing::Span::current().record("transport", "responses_http");

        let body = if request.store && self.session.provider().is_azure_responses_endpoint() {
            let mut body = serde_json::to_value(&request).map_err(|e| {
                ApiError::Stream(format!("failed to encode responses request: {e}"))
            })?;
            attach_item_ids(&mut body, &request.input);
            serde_json::to_vec(&body)
        } else {
            serde_json::to_vec(&request)
        }
        .map(Bytes::from)
        .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;
        drop(request);
        debug!(
            target: "chaos_parrot::request_body",
            body = %String::from_utf8_lossy(&body),
            "responses api request body"
        );

        self.stream_encoded(body, headers, options.compression, options.turn_state)
            .await
    }

    fn path() -> &'static str {
        "responses"
    }

    pub async fn stream(
        &self,
        body: Value,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let encoded = serde_json::to_vec(&body)
            .map(Bytes::from)
            .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;
        drop(body);
        self.stream_encoded(encoded, extra_headers, compression, turn_state)
            .await
    }

    #[instrument(
        name = "responses.stream",
        level = "info",
        skip_all,
        fields(
            transport = "responses_http",
            http.method = "POST",
            api.path = "responses",
            turn.has_state = turn_state.is_some()
        )
    )]
    async fn stream_encoded(
        &self,
        body: Bytes,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let request_compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };

        let stream_response = self
            .session
            .stream_with(
                Method::POST,
                Self::path(),
                extra_headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        rama::http::header::ACCEPT,
                        HeaderValue::from_static(crate::common::MIME_TEXT_EVENT_STREAM),
                    );
                    req.compression = request_compression;
                },
            )
            .await?;

        let use_openai_codex_rate_limits =
            self.session.provider().name.eq_ignore_ascii_case("openai");
        Ok(spawn_response_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            turn_state,
            use_openai_codex_rate_limits,
        ))
    }
}

fn responses_headers(options: &ResponsesOptions) -> HeaderMap {
    let mut headers = options.extra_headers.clone();
    if let Some(ref conv_id) = options.conversation_id {
        insert_header(&mut headers, "x-client-request-id", conv_id);
    }
    headers.extend(build_conversation_headers(options.conversation_id.clone()));
    if let Some(subagent) = subagent_header(&options.session_source) {
        insert_header(&mut headers, "x-openai-subagent", &subagent);
    }
    headers
}
