use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rama::Service;
use rama::error::extra::OpaqueError;
use rama::http::client::EasyHttpWebClient;
use rama::http::{
    Body, Request, Response, StatusCode,
    body::util::BodyExt,
    header::{ACCEPT, CONTENT_TYPE},
};
use rama::service::BoxService;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;
use url::Url;

use crate::error::GuestError;
use crate::protocol::{InitializeResult, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse};
use crate::transport::{MessageTransport, TransportFuture};

const HEADER_SESSION_ID: &str = "Mcp-Session-Id";
const HEADER_PROTOCOL_VERSION: &str = "Mcp-Protocol-Version";
const HEADER_LAST_EVENT_ID: &str = "Last-Event-ID";

type HttpClient = BoxService<Request, Response, OpaqueError>;

#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    pub endpoint: Url,
    pub open_sse_stream: bool,
    pub reconnect_delay: Duration,
    pub default_headers: Vec<(String, String)>,
}

impl HttpClientConfig {
    pub fn new(endpoint: Url) -> Self {
        Self {
            endpoint,
            open_sse_stream: true,
            reconnect_delay: Duration::from_millis(500),
            default_headers: Vec::new(),
        }
    }
}

pub struct HttpTransport {
    inner: Arc<HttpTransportInner>,
}

struct HttpTransportInner {
    client: HttpClient,
    endpoint: Url,
    open_sse_stream: bool,
    request_lock: Mutex<()>,
    recovery_lock: Mutex<()>,
    reconnect_delay: Mutex<Duration>,
    default_headers: Vec<(String, String)>,
    inbound_tx: mpsc::Sender<JsonRpcMessage>,
    inbound_rx: Mutex<mpsc::Receiver<JsonRpcMessage>>,
    cached_initialize: Mutex<Option<JsonRpcMessage>>,
    session_id: Mutex<Option<String>>,
    negotiated_version: Mutex<Option<String>>,
    last_event_id: Mutex<Option<String>>,
    sse_task: Mutex<Option<JoinHandle<()>>>,
    initialized_sent: AtomicBool,
    sse_disabled: AtomicBool,
    closed: AtomicBool,
    initialize_ready: Notify,
    shutdown_notify: Notify,
}

impl HttpTransport {
    pub fn new(config: HttpClientConfig) -> Arc<Self> {
        Self::with_client(config, EasyHttpWebClient::default().boxed())
    }

    fn with_client(config: HttpClientConfig, client: HttpClient) -> Arc<Self> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        Arc::new(Self {
            inner: Arc::new(HttpTransportInner {
                client,
                endpoint: config.endpoint,
                open_sse_stream: config.open_sse_stream,
                request_lock: Mutex::new(()),
                recovery_lock: Mutex::new(()),
                reconnect_delay: Mutex::new(config.reconnect_delay),
                default_headers: config.default_headers,
                inbound_tx,
                inbound_rx: Mutex::new(inbound_rx),
                cached_initialize: Mutex::new(None),
                session_id: Mutex::new(None),
                negotiated_version: Mutex::new(None),
                last_event_id: Mutex::new(None),
                sse_task: Mutex::new(None),
                initialized_sent: AtomicBool::new(false),
                sse_disabled: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                initialize_ready: Notify::new(),
                shutdown_notify: Notify::new(),
            }),
        })
    }
}

impl HttpTransportInner {
    async fn send_message(self: Arc<Self>, message: JsonRpcMessage) -> Result<(), GuestError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(GuestError::Disconnected);
        }

        let _request_guard = self.request_lock.lock().await;
        self.send_message_locked(message, true, true).await
    }

    async fn send_message_locked(
        self: &Arc<Self>,
        message: JsonRpcMessage,
        allow_recovery: bool,
        deliver_inbound: bool,
    ) -> Result<(), GuestError> {
        match self.send_message_once(&message, deliver_inbound).await {
            Err(GuestError::SessionExpired)
                if allow_recovery && !is_initialize_request(&message) =>
            {
                self.recover_session_locked().await?;
                self.send_message_once(&message, deliver_inbound).await?;
            }
            result => result?,
        }

        if is_initialized_notification(&message) {
            self.initialized_sent.store(true, Ordering::SeqCst);
            self.ensure_sse_task().await;
        }

        Ok(())
    }

    async fn send_message_once(
        self: &Arc<Self>,
        message: &JsonRpcMessage,
        deliver_inbound: bool,
    ) -> Result<(), GuestError> {
        let initialize_request = is_initialize_request(&message);
        if initialize_request {
            *self.cached_initialize.lock().await = Some(message.clone());
        }

        let request = self.build_post_request(message, initialize_request).await?;
        let response = self
            .client
            .serve(request)
            .await
            .map_err(|error| GuestError::Http(error.to_string()))?;

        self.handle_post_response(response, initialize_request, deliver_inbound)
            .await
    }

    async fn build_post_request(
        &self,
        message: &JsonRpcMessage,
        initialize_request: bool,
    ) -> Result<Request, GuestError> {
        let body = serde_json::to_vec(message)?;
        let mut builder = Request::builder()
            .method("POST")
            .uri(self.endpoint.as_str())
            .header(CONTENT_TYPE, "application/json");

        if !initialize_request {
            if let Some(session_id) = self.session_id.lock().await.clone() {
                builder = builder.header(HEADER_SESSION_ID, session_id);
            }
            if let Some(version) = self.negotiated_version.lock().await.clone() {
                builder = builder.header(HEADER_PROTOCOL_VERSION, version);
            }
        }

        builder = self.apply_default_headers(
            builder,
            Some("application/json, text/event-stream"),
            &[
                CONTENT_TYPE.as_str(),
                HEADER_SESSION_ID,
                HEADER_PROTOCOL_VERSION,
                HEADER_LAST_EVENT_ID,
            ],
        );

        builder
            .body(Body::from(body))
            .map_err(|error| GuestError::Http(error.to_string()))
    }

    async fn handle_post_response(
        self: &Arc<Self>,
        response: Response,
        initialize_request: bool,
        deliver_inbound: bool,
    ) -> Result<(), GuestError> {
        self.capture_session_id(response.headers()).await?;

        match response.status() {
            StatusCode::OK => {}
            StatusCode::ACCEPTED | StatusCode::NO_CONTENT => return Ok(()),
            StatusCode::NOT_FOUND => {
                self.clear_session_state().await;
                return Err(GuestError::SessionExpired);
            }
            status => {
                let body = collect_body_string(response).await;
                return Err(GuestError::Http(format!(
                    "http POST {} returned {}{}",
                    self.endpoint,
                    status,
                    format_body_suffix(body.as_deref()),
                )));
            }
        }

        let content_type = header_value(response.headers(), CONTENT_TYPE).unwrap_or_default();
        if content_type.starts_with("application/json") {
            let bytes = collect_body_bytes(response).await?;
            if bytes.is_empty() {
                return Ok(());
            }

            let message: JsonRpcMessage = serde_json::from_slice(&bytes)?;
            self.inspect_inbound_message(&message, initialize_request)
                .await?;
            if deliver_inbound {
                self.enqueue_message(message).await?;
            }
            Ok(())
        } else if content_type.starts_with("text/event-stream") {
            let this = Arc::clone(self);
            tokio::spawn(async move {
                if let Err(error) = this
                    .consume_sse_response(response, false, initialize_request, deliver_inbound)
                    .await
                {
                    tracing::warn!(error = %error, "post sse stream failed");
                }
            });
            Ok(())
        } else {
            Err(GuestError::Http(format!(
                "http POST {} returned unsupported content-type {}",
                self.endpoint, content_type
            )))
        }
    }

    async fn inspect_inbound_message(
        self: &Arc<Self>,
        message: &JsonRpcMessage,
        initialize_request: bool,
    ) -> Result<(), GuestError> {
        if !initialize_request {
            return Ok(());
        }

        let JsonRpcMessage::Response(JsonRpcResponse {
            id,
            result: Some(result),
            error: None,
            ..
        }) = message
        else {
            return Ok(());
        };

        if *id != serde_json::json!(1) {
            return Ok(());
        }

        let initialize: InitializeResult = serde_json::from_value(result.clone())?;
        *self.negotiated_version.lock().await = Some(initialize.protocol_version);
        self.initialize_ready.notify_waiters();
        Ok(())
    }

    async fn ensure_sse_task(self: &Arc<Self>) {
        if !self.open_sse_stream
            || self.sse_disabled.load(Ordering::Relaxed)
            || self.closed.load(Ordering::Relaxed)
        {
            return;
        }

        if self.session_id.lock().await.is_none() {
            return;
        }

        let mut guard = self.sse_task.lock().await;
        if let Some(handle) = guard.as_ref() {
            if !handle.is_finished() {
                return;
            }
        }

        let this = Arc::clone(self);
        *guard = Some(tokio::spawn(async move {
            this.run_sse_loop().await;
        }));
    }

    async fn run_sse_loop(self: Arc<Self>) {
        loop {
            if self.closed.load(Ordering::Relaxed) || self.sse_disabled.load(Ordering::Relaxed) {
                break;
            }

            let request = match self.build_get_request().await {
                Ok(request) => request,
                Err(error) => {
                    tracing::debug!(error = %error, "sse stream unavailable");
                    break;
                }
            };

            let response = match self.client.serve(request).await {
                Ok(response) => response,
                Err(error) => {
                    tracing::warn!(error = %error, "sse connection failed");
                    if !self.wait_for_retry().await {
                        break;
                    }
                    continue;
                }
            };

            self.capture_session_id(response.headers()).await.ok();

            match response.status() {
                StatusCode::OK => {
                    if let Some(content_type) = header_value(response.headers(), CONTENT_TYPE) {
                        if !content_type.starts_with("text/event-stream") {
                            tracing::warn!(content_type, "unexpected sse content type");
                            break;
                        }
                    }

                    if let Err(error) = self.consume_sse_response(response, true, false, true).await
                    {
                        tracing::warn!(error = %error, "sse stream ended with error");
                    }
                }
                StatusCode::METHOD_NOT_ALLOWED => {
                    self.sse_disabled.store(true, Ordering::Relaxed);
                    break;
                }
                StatusCode::NOT_FOUND => {
                    self.clear_session_state().await;
                    break;
                }
                status => {
                    let body = collect_body_string(response).await;
                    tracing::warn!(
                        status = %status,
                        body = %body.unwrap_or_default(),
                        "sse connection rejected"
                    );
                }
            }

            if !self.wait_for_retry().await {
                break;
            }
        }
    }

    async fn build_get_request(&self) -> Result<Request, GuestError> {
        let session_id = self
            .session_id
            .lock()
            .await
            .clone()
            .ok_or_else(|| GuestError::Protocol("http session not initialized".to_string()))?;

        let mut builder = Request::builder()
            .method("GET")
            .uri(self.endpoint.as_str())
            .header(HEADER_SESSION_ID, session_id);

        if let Some(version) = self.negotiated_version.lock().await.clone() {
            builder = builder.header(HEADER_PROTOCOL_VERSION, version);
        }

        if let Some(last_event_id) = self.last_event_id.lock().await.clone() {
            builder = builder.header(HEADER_LAST_EVENT_ID, last_event_id);
        }

        builder = self.apply_default_headers(
            builder,
            Some("text/event-stream"),
            &[
                HEADER_SESSION_ID,
                HEADER_PROTOCOL_VERSION,
                HEADER_LAST_EVENT_ID,
            ],
        );

        builder
            .body(Body::empty())
            .map_err(|error| GuestError::Http(error.to_string()))
    }

    async fn consume_sse_response(
        self: &Arc<Self>,
        response: Response,
        track_last_event_id: bool,
        initialize_request_context: bool,
        deliver_inbound: bool,
    ) -> Result<(), GuestError> {
        let mut stream = response.into_body().into_string_data_event_stream();

        while let Some(event) = stream.next().await {
            let event = event.map_err(|error| GuestError::Http(error.to_string()))?;
            if let Some(retry) = event.retry() {
                *self.reconnect_delay.lock().await = retry;
            }
            if track_last_event_id {
                *self.last_event_id.lock().await = event.id().map(ToOwned::to_owned);
            }
            let Some(data) = event.into_data() else {
                continue;
            };
            if data.trim().is_empty() {
                continue;
            }
            let message: JsonRpcMessage = serde_json::from_str(&data)?;
            self.inspect_inbound_message(&message, initialize_request_context)
                .await?;
            if deliver_inbound {
                self.enqueue_message(message).await?;
            }
        }

        Ok(())
    }

    async fn enqueue_message(&self, message: JsonRpcMessage) -> Result<(), GuestError> {
        self.inbound_tx
            .send(message)
            .await
            .map_err(|_| GuestError::Disconnected)
    }

    async fn wait_for_retry(&self) -> bool {
        let retry_delay = *self.reconnect_delay.lock().await;
        tokio::select! {
            _ = tokio::time::sleep(retry_delay) => true,
            _ = self.shutdown_notify.notified() => false,
        }
    }

    async fn capture_session_id(&self, headers: &rama::http::HeaderMap) -> Result<(), GuestError> {
        if let Some(session_id) = header_value(headers, HEADER_SESSION_ID) {
            *self.session_id.lock().await = Some(session_id.to_string());
        }
        Ok(())
    }

    async fn clear_session_state(&self) {
        *self.session_id.lock().await = None;
        *self.negotiated_version.lock().await = None;
        *self.last_event_id.lock().await = None;
    }

    async fn recover_session_locked(self: &Arc<Self>) -> Result<(), GuestError> {
        let _recovery_guard = self.recovery_lock.lock().await;

        if self.closed.load(Ordering::Relaxed) {
            return Err(GuestError::Disconnected);
        }

        let initialize_message = self
            .cached_initialize
            .lock()
            .await
            .clone()
            .ok_or(GuestError::SessionExpired)?;
        let initialized_sent = self.initialized_sent.load(Ordering::SeqCst);

        self.stop_sse_task().await;
        self.clear_session_state().await;

        self.send_message_once(&initialize_message, false).await?;
        self.wait_for_session_ready(Duration::from_secs(10)).await?;

        if initialized_sent {
            let initialized = JsonRpcMessage::Notification(JsonRpcRequest::notification(
                "notifications/initialized",
                None,
            ));
            self.send_message_once(&initialized, true).await?;
            self.initialized_sent.store(true, Ordering::SeqCst);
            self.ensure_sse_task().await;
        }

        Ok(())
    }

    async fn wait_for_session_ready(&self, timeout: Duration) -> Result<(), GuestError> {
        if self.has_active_session().await {
            return Ok(());
        }

        tokio::time::timeout(timeout, async {
            loop {
                self.initialize_ready.notified().await;
                if self.has_active_session().await {
                    break;
                }
            }
        })
        .await
        .map_err(|_| GuestError::Timeout(timeout))?;

        Ok(())
    }

    async fn has_active_session(&self) -> bool {
        self.session_id.lock().await.is_some() && self.negotiated_version.lock().await.is_some()
    }

    async fn stop_sse_task(&self) {
        if let Some(handle) = self.sse_task.lock().await.take() {
            handle.abort();
        }
    }

    async fn shutdown_inner(self: Arc<Self>) -> Result<(), GuestError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        self.shutdown_notify.notify_waiters();

        self.stop_sse_task().await;

        let session_id = self.session_id.lock().await.clone();
        let protocol_version = self.negotiated_version.lock().await.clone();

        if let Some(session_id) = session_id {
            let mut builder = Request::builder()
                .method("DELETE")
                .uri(self.endpoint.as_str())
                .header(HEADER_SESSION_ID, session_id);

            if let Some(version) = protocol_version {
                builder = builder.header(HEADER_PROTOCOL_VERSION, version);
            }

            builder = self.apply_default_headers(
                builder,
                None,
                &[
                    HEADER_SESSION_ID,
                    HEADER_PROTOCOL_VERSION,
                    HEADER_LAST_EVENT_ID,
                ],
            );

            if let Ok(request) = builder.body(Body::empty()) {
                let _ = self.client.serve(request).await;
            }
        }

        Ok(())
    }

    fn apply_default_headers(
        &self,
        mut builder: rama::http::request::Builder,
        required_accept: Option<&str>,
        protected_headers: &[&str],
    ) -> rama::http::request::Builder {
        let mut custom_accept: Option<String> = None;

        for (name, value) in &self.default_headers {
            if protected_headers
                .iter()
                .any(|header| name.eq_ignore_ascii_case(header))
            {
                continue;
            }

            if name.eq_ignore_ascii_case(ACCEPT.as_str()) {
                match &mut custom_accept {
                    Some(existing) if !value.trim().is_empty() => {
                        existing.push_str(", ");
                        existing.push_str(value);
                    }
                    Some(_) => {}
                    None if !value.trim().is_empty() => custom_accept = Some(value.clone()),
                    None => {}
                }
                continue;
            }

            builder = builder.header(name.as_str(), value.as_str());
        }

        match (required_accept, custom_accept) {
            (Some(required), Some(custom)) if !custom.trim().is_empty() => {
                builder.header(ACCEPT, format!("{required}, {custom}"))
            }
            (Some(required), _) => builder.header(ACCEPT, required),
            (None, Some(custom)) if !custom.trim().is_empty() => builder.header(ACCEPT, custom),
            _ => builder,
        }
    }
}

impl MessageTransport for HttpTransport {
    fn send<'a>(&'a self, message: JsonRpcMessage) -> TransportFuture<'a, ()> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.send_message(message).await })
    }

    fn recv<'a>(&'a self) -> TransportFuture<'a, JsonRpcMessage> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            if inner.closed.load(Ordering::Relaxed) {
                return Err(GuestError::Disconnected);
            }

            let mut receiver = inner.inbound_rx.lock().await;
            tokio::select! {
                message = receiver.recv() => {
                    message.ok_or(GuestError::Disconnected)
                }
                _ = inner.shutdown_notify.notified() => Err(GuestError::Disconnected),
            }
        })
    }

    fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.shutdown_inner().await })
    }
}

fn is_initialize_request(message: &JsonRpcMessage) -> bool {
    matches!(
        message,
        JsonRpcMessage::Request(request) if request.method == "initialize"
    )
}

fn is_initialized_notification(message: &JsonRpcMessage) -> bool {
    matches!(
        message,
        JsonRpcMessage::Notification(notification)
            if notification.method == "notifications/initialized"
    )
}

fn header_value<'a>(headers: &'a rama::http::HeaderMap, name: impl AsRef<str>) -> Option<&'a str> {
    headers
        .get(name.as_ref())
        .and_then(|value| value.to_str().ok())
}

async fn collect_body_bytes(response: Response) -> Result<Vec<u8>, GuestError> {
    let collected = response
        .into_body()
        .collect()
        .await
        .map_err(|error| GuestError::Http(error.to_string()))?;
    Ok(collected.to_bytes().to_vec())
}

async fn collect_body_string(response: Response) -> Option<String> {
    collect_body_bytes(response)
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|body| !body.trim().is_empty())
}

fn format_body_suffix(body: Option<&str>) -> String {
    body.map(|body| format!(": {body}")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rama::bytes::Bytes;
    use rama::http::body::util::BodyExt as _;
    use rama::service::service_fn;
    use serde_json::json;
    use tokio::sync::Mutex as AsyncMutex;

    use super::*;
    use crate::protocol::{
        ClientCapabilities, Implementation, InitializeRequest, JsonRpcRequest, ServerCapabilities,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SeenRequest {
        http_method: String,
        rpc_method: Option<String>,
        session_id: Option<String>,
    }

    async fn record_request(req: Request) -> SeenRequest {
        let http_method = req.method().as_str().to_string();
        let session_id = req
            .headers()
            .get(HEADER_SESSION_ID)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let rpc_method = if http_method == "POST" {
            let body = req.into_body().collect().await.unwrap().to_bytes();
            let value: serde_json::Value = serde_json::from_slice(body.as_ref()).unwrap();
            value
                .get("method")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        } else {
            None
        };

        SeenRequest {
            http_method,
            rpc_method,
            session_id,
        }
    }

    fn initialize_http_response(session_id: &str) -> Response {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .header(HEADER_SESSION_ID, session_id)
            .body(Body::from(
                serde_json::to_vec(&initialize_response()).unwrap(),
            ))
            .unwrap()
    }

    fn initialize_response() -> JsonRpcMessage {
        JsonRpcMessage::Response(JsonRpcResponse::success(
            json!(1),
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": ServerCapabilities::default(),
                "serverInfo": {
                    "name": "test-server",
                    "version": "0.1.0"
                }
            }),
        ))
    }

    #[tokio::test]
    async fn transport_posts_initialize_response() {
        let response = serde_json::to_vec(&initialize_response()).unwrap();
        let client = service_fn(move |_req: Request| {
            let response = response.clone();
            async move {
                Ok::<_, OpaqueError>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(CONTENT_TYPE, "application/json")
                        .header(HEADER_SESSION_ID, "session-123")
                        .body(Body::from(response))
                        .unwrap(),
                )
            }
        })
        .boxed();

        let transport = HttpTransport::with_client(
            HttpClientConfig {
                endpoint: Url::parse("http://localhost:62770/mcp").unwrap(),
                open_sse_stream: false,
                reconnect_delay: Duration::from_millis(5),
                default_headers: Vec::new(),
            },
            client,
        );

        transport
            .send(JsonRpcMessage::Request(JsonRpcRequest::new(
                json!(1),
                "initialize",
                Some(
                    serde_json::to_value(InitializeRequest {
                        protocol_version: "2025-11-25".to_string(),
                        capabilities: ClientCapabilities::default(),
                        client_info: Implementation::new("mcp-guest", "0.1.0"),
                    })
                    .unwrap(),
                ),
            )))
            .await
            .unwrap();

        let message = transport.recv().await.unwrap();
        let JsonRpcMessage::Response(response) = message else {
            panic!("expected initialize response");
        };
        assert_eq!(response.id, json!(1));
        assert!(response.error.is_none());
        let result = response.result.expect("initialize result");
        assert_eq!(result["protocolVersion"], json!("2025-11-25"));
        assert_eq!(result["serverInfo"]["name"], json!("test-server"));
        assert_eq!(
            transport.inner.session_id.lock().await.as_deref(),
            Some("session-123")
        );
        assert_eq!(
            transport.inner.negotiated_version.lock().await.as_deref(),
            Some("2025-11-25")
        );
    }

    #[tokio::test]
    async fn transport_reads_get_sse_notifications() {
        let get_count = Arc::new(AtomicUsize::new(0));
        let client = {
            let get_count = Arc::clone(&get_count);
            service_fn(move |req: Request| {
                let get_count = Arc::clone(&get_count);
                async move {
                    let response = match req.method().as_str() {
                        "POST" => match record_request(req).await.rpc_method.as_deref() {
                            Some("initialize") => initialize_http_response("session-123"),
                            Some("notifications/initialized") => Response::builder()
                                .status(StatusCode::ACCEPTED)
                                .body(Body::empty())
                                .unwrap(),
                            other => Response::builder()
                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                .body(Body::from(format!("unexpected rpc method {other:?}")))
                                .unwrap(),
                        },
                        "GET" => {
                            if get_count.fetch_add(1, Ordering::Relaxed) == 0 {
                                let body = Body::from_stream(tokio_stream::iter(vec![Ok::<_, OpaqueError>(
                                    Bytes::from(
                                        "id: session-123-1\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/resources/list_changed\"}\n\n",
                                    ),
                                )]));
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .header(CONTENT_TYPE, "text/event-stream")
                                    .body(body)
                                    .unwrap()
                            } else {
                                Response::builder()
                                    .status(StatusCode::METHOD_NOT_ALLOWED)
                                    .body(Body::empty())
                                    .unwrap()
                            }
                        }
                        method => Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(method.to_string()))
                            .unwrap(),
                    };

                    Ok::<_, OpaqueError>(response)
                }
            })
            .boxed()
        };

        let transport = HttpTransport::with_client(
            HttpClientConfig {
                endpoint: Url::parse("http://localhost:62770/mcp").unwrap(),
                open_sse_stream: true,
                reconnect_delay: Duration::from_millis(5),
                default_headers: Vec::new(),
            },
            client,
        );

        transport
            .send(JsonRpcMessage::Request(JsonRpcRequest::new(
                json!(1),
                "initialize",
                Some(
                    serde_json::to_value(InitializeRequest {
                        protocol_version: "2025-11-25".to_string(),
                        capabilities: ClientCapabilities::default(),
                        client_info: Implementation::new("mcp-guest", "0.1.0"),
                    })
                    .unwrap(),
                ),
            )))
            .await
            .unwrap();

        let first = transport.recv().await.unwrap();
        let JsonRpcMessage::Response(response) = first else {
            panic!("expected initialize response");
        };
        assert_eq!(response.id, json!(1));
        assert!(response.error.is_none());

        transport
            .send(JsonRpcMessage::Notification(JsonRpcRequest::notification(
                "notifications/initialized",
                None,
            )))
            .await
            .unwrap();

        let second = tokio::time::timeout(Duration::from_secs(1), transport.recv())
            .await
            .unwrap()
            .unwrap();
        let JsonRpcMessage::Notification(notification) = second else {
            panic!("expected resources/list_changed notification");
        };
        assert_eq!(notification.method, "notifications/resources/list_changed");
        assert!(notification.params.is_none());
    }

    #[tokio::test]
    async fn transport_recovers_session_on_404_and_retries_request() {
        let seen_requests = Arc::new(AsyncMutex::new(Vec::<SeenRequest>::new()));
        let call_count = Arc::new(AtomicUsize::new(0));

        let client = {
            let seen_requests = Arc::clone(&seen_requests);
            let call_count = Arc::clone(&call_count);
            service_fn(move |req: Request| {
                let seen_requests = Arc::clone(&seen_requests);
                let call_count = Arc::clone(&call_count);
                async move {
                    let seen = record_request(req).await;
                    seen_requests.lock().await.push(seen);

                    let response = match call_count.fetch_add(1, Ordering::Relaxed) {
                        0 => initialize_http_response("session-1"),
                        1 => Response::builder()
                            .status(StatusCode::ACCEPTED)
                            .body(Body::empty())
                            .unwrap(),
                        2 => Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Body::empty())
                            .unwrap(),
                        3 => initialize_http_response("session-2"),
                        4 => Response::builder()
                            .status(StatusCode::ACCEPTED)
                            .header(HEADER_SESSION_ID, "session-2")
                            .body(Body::empty())
                            .unwrap(),
                        5 => Response::builder()
                            .status(StatusCode::OK)
                            .header(CONTENT_TYPE, "application/json")
                            .header(HEADER_SESSION_ID, "session-2")
                            .body(Body::from(
                                serde_json::to_vec(&JsonRpcMessage::Response(
                                    JsonRpcResponse::success(json!(2), json!({})),
                                ))
                                .unwrap(),
                            ))
                            .unwrap(),
                        other => Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(format!("unexpected call {other}")))
                            .unwrap(),
                    };

                    Ok::<_, OpaqueError>(response)
                }
            })
            .boxed()
        };

        let transport = HttpTransport::with_client(
            HttpClientConfig {
                endpoint: Url::parse("http://localhost:62770/mcp").unwrap(),
                open_sse_stream: false,
                reconnect_delay: Duration::from_millis(5),
                default_headers: Vec::new(),
            },
            client,
        );

        transport
            .send(JsonRpcMessage::Request(JsonRpcRequest::new(
                json!(1),
                "initialize",
                Some(
                    serde_json::to_value(InitializeRequest {
                        protocol_version: "2025-11-25".to_string(),
                        capabilities: ClientCapabilities::default(),
                        client_info: Implementation::new("mcp-guest", "0.1.0"),
                    })
                    .unwrap(),
                ),
            )))
            .await
            .unwrap();
        let _ = transport.recv().await.unwrap();

        transport
            .send(JsonRpcMessage::Notification(JsonRpcRequest::notification(
                "notifications/initialized",
                None,
            )))
            .await
            .unwrap();

        transport
            .send(JsonRpcMessage::Request(JsonRpcRequest::new(
                json!(2),
                "ping",
                Some(json!({})),
            )))
            .await
            .unwrap();

        let message = transport.recv().await.unwrap();
        let JsonRpcMessage::Response(response) = message else {
            panic!("expected retried ping response");
        };
        assert_eq!(response.id, json!(2));
        assert!(response.error.is_none());

        let seen = seen_requests.lock().await.clone();
        assert_eq!(
            seen,
            vec![
                SeenRequest {
                    http_method: "POST".to_string(),
                    rpc_method: Some("initialize".to_string()),
                    session_id: None,
                },
                SeenRequest {
                    http_method: "POST".to_string(),
                    rpc_method: Some("notifications/initialized".to_string()),
                    session_id: Some("session-1".to_string()),
                },
                SeenRequest {
                    http_method: "POST".to_string(),
                    rpc_method: Some("ping".to_string()),
                    session_id: Some("session-1".to_string()),
                },
                SeenRequest {
                    http_method: "POST".to_string(),
                    rpc_method: Some("initialize".to_string()),
                    session_id: None,
                },
                SeenRequest {
                    http_method: "POST".to_string(),
                    rpc_method: Some("notifications/initialized".to_string()),
                    session_id: Some("session-2".to_string()),
                },
                SeenRequest {
                    http_method: "POST".to_string(),
                    rpc_method: Some("ping".to_string()),
                    session_id: Some("session-2".to_string()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn transport_applies_custom_headers_and_merges_accept() {
        let client = service_fn(|_req: Request| async move {
            Ok::<_, OpaqueError>(
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::empty())
                    .unwrap(),
            )
        })
        .boxed();

        let transport = HttpTransport::with_client(
            HttpClientConfig {
                endpoint: Url::parse("http://localhost:62770/mcp").unwrap(),
                open_sse_stream: false,
                reconnect_delay: Duration::from_millis(5),
                default_headers: vec![
                    ("Authorization".to_string(), "Bearer secret".to_string()),
                    ("X-Test".to_string(), "1".to_string()),
                    (
                        "Accept".to_string(),
                        "application/vnd.example+json".to_string(),
                    ),
                    ("Content-Type".to_string(), "text/plain".to_string()),
                    (HEADER_SESSION_ID.to_string(), "ignored".to_string()),
                ],
            },
            client,
        );

        let post_request = transport
            .inner
            .build_post_request(
                &JsonRpcMessage::Request(JsonRpcRequest::new(json!(1), "initialize", None)),
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            post_request.headers()["authorization"].to_str().unwrap(),
            "Bearer secret"
        );
        assert_eq!(post_request.headers()["x-test"].to_str().unwrap(), "1");
        assert_eq!(
            post_request.headers()[CONTENT_TYPE].to_str().unwrap(),
            "application/json"
        );
        let post_accept = post_request.headers()[ACCEPT].to_str().unwrap();
        assert!(post_accept.contains("application/json"));
        assert!(post_accept.contains("text/event-stream"));
        assert!(post_accept.contains("application/vnd.example+json"));
        assert!(post_request.headers().get(HEADER_SESSION_ID).is_none());

        *transport.inner.session_id.lock().await = Some("session-123".to_string());
        *transport.inner.negotiated_version.lock().await = Some("2025-11-25".to_string());
        *transport.inner.last_event_id.lock().await = Some("event-42".to_string());

        let get_request = transport.inner.build_get_request().await.unwrap();
        assert_eq!(
            get_request.headers()["authorization"].to_str().unwrap(),
            "Bearer secret"
        );
        assert_eq!(get_request.headers()["x-test"].to_str().unwrap(), "1");
        assert_eq!(
            get_request.headers()[HEADER_SESSION_ID].to_str().unwrap(),
            "session-123"
        );
        assert_eq!(
            get_request.headers()[HEADER_PROTOCOL_VERSION]
                .to_str()
                .unwrap(),
            "2025-11-25"
        );
        assert_eq!(
            get_request.headers()[HEADER_LAST_EVENT_ID]
                .to_str()
                .unwrap(),
            "event-42"
        );
        let get_accept = get_request.headers()[ACCEPT].to_str().unwrap();
        assert!(get_accept.contains("text/event-stream"));
        assert!(get_accept.contains("application/vnd.example+json"));
    }
}
