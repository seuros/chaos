use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono_machines::ExponentialBackoff;
use chrono_machines::backoff::BackoffStrategy;
use rama::Service;
use rama::error::extra::OpaqueError;
use rama::http::Body;
use rama::http::Request;
use rama::http::Response;
use rama::http::StatusCode;
use rama::http::body::util::BodyExt;
use rama::http::client::EasyHttpWebClient;
use rama::http::header::ACCEPT;
use rama::http::header::CONTENT_TYPE;
use rama::service::BoxService;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use url::Url;

use crate::error::GuestError;
use crate::protocol::InitializeResult;

const MIME_APPLICATION_JSON: &str = "application/json";
const MIME_TEXT_EVENT_STREAM: &str = "text/event-stream";
use crate::protocol::JsonRpcMessage;
use crate::protocol::JsonRpcRequest;
use crate::protocol::JsonRpcResponse;
use crate::transport::MessageTransport;
use crate::transport::TransportFuture;

const HEADER_SESSION_ID: &str = "Mcp-Session-Id";
const HEADER_PROTOCOL_VERSION: &str = "Mcp-Protocol-Version";
const HEADER_LAST_EVENT_ID: &str = "Last-Event-ID";

type HttpClient = BoxService<Request, Response, OpaqueError>;

mod lifecycle;
use lifecycle::{HttpSessionEvent, HttpSseEvent, Lifecycle};

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
    recovery_lock: Mutex<()>,
    default_headers: Vec<(String, String)>,
    inbound_tx: mpsc::Sender<JsonRpcMessage>,
    inbound_rx: Mutex<mpsc::Receiver<JsonRpcMessage>>,
    lifecycle: std::sync::Mutex<Lifecycle>,
    closed: AtomicBool,
    session_generation: AtomicU64,
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
                recovery_lock: Mutex::new(()),
                default_headers: config.default_headers,
                inbound_tx,
                inbound_rx: Mutex::new(inbound_rx),
                lifecycle: std::sync::Mutex::new(Lifecycle::new(config.reconnect_delay)),
                closed: AtomicBool::new(false),
                session_generation: AtomicU64::new(0),
                initialize_ready: Notify::new(),
                shutdown_notify: Notify::new(),
            }),
        })
    }
}

impl Drop for HttpTransport {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.lifecycle().close();
        self.inner.shutdown_notify.notify_waiters();
        self.inner.initialize_ready.notify_waiters();
    }
}

impl HttpTransportInner {
    fn lifecycle(&self) -> std::sync::MutexGuard<'_, Lifecycle> {
        self.lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn current_lifecycle(
        &self,
        generation: u64,
    ) -> Result<std::sync::MutexGuard<'_, Lifecycle>, GuestError> {
        let lifecycle = self.lifecycle();
        if self.closed.load(Ordering::Acquire) {
            return Err(GuestError::Disconnected);
        }
        if self.session_generation.load(Ordering::Acquire) != generation {
            return Err(GuestError::SessionExpired);
        }
        Ok(lifecycle)
    }

    async fn send_message(self: Arc<Self>, message: JsonRpcMessage) -> Result<(), GuestError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(GuestError::Disconnected);
        }

        self.send_message_with_recovery(message, true, true).await
    }

    async fn send_message_with_recovery(
        self: &Arc<Self>,
        message: JsonRpcMessage,
        allow_recovery: bool,
        deliver_inbound: bool,
    ) -> Result<(), GuestError> {
        let generation = self.session_generation.load(Ordering::Acquire);
        match self.send_message_once(&message, deliver_inbound).await {
            Err(GuestError::SessionExpired)
                if allow_recovery && !is_initialize_request(&message) =>
            {
                self.recover_session(generation).await?;
                self.send_message_once(&message, deliver_inbound).await?;
            }
            result => result?,
        }

        if is_initialized_notification(&message) {
            if !self.lifecycle().session(HttpSessionEvent::Initialized) {
                return Err(GuestError::Disconnected);
            }
            self.ensure_sse_task().await;
        }

        Ok(())
    }

    async fn send_message_once(
        self: &Arc<Self>,
        message: &JsonRpcMessage,
        deliver_inbound: bool,
    ) -> Result<(), GuestError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(GuestError::Disconnected);
        }
        let initialize_request = is_initialize_request(message);
        if initialize_request {
            let mut lifecycle = self.lifecycle();
            if !lifecycle.session(HttpSessionEvent::BeginInitialization) {
                return Err(GuestError::Disconnected);
            }
            lifecycle
                .data_mut()
                .ok_or(GuestError::Disconnected)?
                .cached_initialize = Some(message.clone());
        }

        let generation = self.session_generation.load(Ordering::Acquire);
        let request = self.build_post_request(message, initialize_request).await?;
        let response = self.client.serve(request).await.map_err(http_error)?;

        self.handle_post_response(response, initialize_request, deliver_inbound, generation)
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
            .header(CONTENT_TYPE, MIME_APPLICATION_JSON);

        if !initialize_request {
            let lifecycle = self.lifecycle();
            let data = lifecycle.data().ok_or(GuestError::Disconnected)?;
            if let Some(session_id) = &data.session_id {
                builder = builder.header(HEADER_SESSION_ID, session_id);
            }
            if let Some(version) = &data.negotiated_version {
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

        builder.body(Body::from(body)).map_err(http_error)
    }

    async fn handle_post_response(
        self: &Arc<Self>,
        response: Response,
        initialize_request: bool,
        deliver_inbound: bool,
        generation: u64,
    ) -> Result<(), GuestError> {
        match self.capture_session_id(response.headers(), generation) {
            Err(GuestError::SessionExpired) if !initialize_request => {}
            result => result?,
        }

        match response.status() {
            StatusCode::OK => {}
            StatusCode::ACCEPTED | StatusCode::NO_CONTENT => return Ok(()),
            StatusCode::NOT_FOUND => {
                self.clear_session_state(generation)?;
                return Err(GuestError::SessionExpired);
            }
            status => {
                let body = collect_body_string(response).await;
                if is_stale_session_rejection(status, body.as_deref()) {
                    self.clear_session_state(generation)?;
                    return Err(GuestError::SessionExpired);
                }
                return Err(GuestError::Http(format!(
                    "http POST {} returned {}{}",
                    self.endpoint,
                    status,
                    format_body_suffix(body.as_deref()),
                )));
            }
        }

        let content_type = header_value(response.headers(), CONTENT_TYPE).unwrap_or_default();
        if content_type.starts_with(MIME_APPLICATION_JSON) {
            let bytes = collect_body_bytes(response).await?;
            if bytes.is_empty() {
                return Ok(());
            }

            let message: JsonRpcMessage = serde_json::from_slice(&bytes)?;
            self.inspect_inbound_message(&message, initialize_request, generation)
                .await?;
            if deliver_inbound {
                self.enqueue_message(message).await?;
            }
            Ok(())
        } else if content_type.starts_with(MIME_TEXT_EVENT_STREAM) {
            let this = Arc::clone(self);
            let mut lifecycle = self.lifecycle();
            let data = lifecycle.data_mut().ok_or(GuestError::Disconnected)?;
            while data.streams.try_join_next().is_some() {}
            data.streams.spawn(async move {
                if let Err(error) = this
                    .consume_sse_response(
                        response,
                        false,
                        initialize_request,
                        deliver_inbound,
                        generation,
                    )
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
        generation: u64,
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

        if *id != Some(serde_json::json!(crate::protocol::INITIALIZE_REQUEST_ID)) {
            return Ok(());
        }

        let initialize: InitializeResult = serde_json::from_value(result.clone())?;
        let mut lifecycle = self.current_lifecycle(generation)?;
        if !lifecycle.session(HttpSessionEvent::Negotiated) {
            return Err(GuestError::Disconnected);
        }
        lifecycle
            .data_mut()
            .ok_or(GuestError::Disconnected)?
            .negotiated_version = Some(initialize.protocol_version);
        self.initialize_ready.notify_waiters();
        Ok(())
    }

    async fn ensure_sse_task(self: &Arc<Self>) {
        let mut lifecycle = self.lifecycle();
        if !self.open_sse_stream || !lifecycle.sse_enabled() || self.closed.load(Ordering::Relaxed)
        {
            return;
        }

        if lifecycle
            .data()
            .is_none_or(|data| data.session_id.is_none())
        {
            return;
        }

        if let Some(handle) = lifecycle.stream().and_then(|stream| stream.task.as_ref())
            && !handle.is_finished()
        {
            return;
        }
        if !lifecycle.sse(HttpSseEvent::Connect) {
            return;
        }

        let this = Arc::clone(self);
        let Some(stream) = lifecycle.stream() else {
            return;
        };
        stream.task = Some(tokio::spawn(async move {
            this.run_sse_loop().await;
        }));
    }

    async fn run_sse_loop(self: Arc<Self>) {
        loop {
            if self.closed.load(Ordering::Relaxed) || !self.lifecycle().sse_enabled() {
                break;
            }
            if !self.lifecycle().sse(HttpSseEvent::Connect) {
                break;
            }

            let generation = self.session_generation.load(Ordering::Acquire);
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

            if self
                .capture_session_id(response.headers(), generation)
                .is_err()
            {
                break;
            }

            match response.status() {
                StatusCode::OK => {
                    if !self.lifecycle().sse(HttpSseEvent::Connected) {
                        break;
                    }
                    if let Some(content_type) = header_value(response.headers(), CONTENT_TYPE)
                        && !content_type.starts_with(MIME_TEXT_EVENT_STREAM)
                    {
                        tracing::warn!(content_type, "unexpected sse content type");
                        break;
                    }

                    if let Some(stream) = self.lifecycle().stream() {
                        stream.reconnect_attempt = 0;
                    }
                    if let Err(error) = self
                        .consume_sse_response(response, true, false, true, generation)
                        .await
                    {
                        tracing::warn!(error = %error, "sse stream ended with error");
                    }
                }
                StatusCode::METHOD_NOT_ALLOWED => {
                    self.lifecycle().sse(HttpSseEvent::Disable);
                    break;
                }
                StatusCode::NOT_FOUND => {
                    let _ = self.clear_session_state(generation);
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
        let lifecycle = self.lifecycle();
        let data = lifecycle.data().ok_or(GuestError::Disconnected)?;
        let session_id = data
            .session_id
            .as_ref()
            .ok_or_else(|| GuestError::Protocol("http session not initialized".to_string()))?;

        let mut builder = Request::builder()
            .method("GET")
            .uri(self.endpoint.as_str())
            .header(HEADER_SESSION_ID, session_id);

        if let Some(version) = &data.negotiated_version {
            builder = builder.header(HEADER_PROTOCOL_VERSION, version);
        }

        if let Some(last_event_id) = &data.last_event_id {
            builder = builder.header(HEADER_LAST_EVENT_ID, last_event_id);
        }

        builder = self.apply_default_headers(
            builder,
            Some(MIME_TEXT_EVENT_STREAM),
            &[
                HEADER_SESSION_ID,
                HEADER_PROTOCOL_VERSION,
                HEADER_LAST_EVENT_ID,
            ],
        );

        builder.body(Body::empty()).map_err(http_error)
    }

    async fn consume_sse_response(
        self: &Arc<Self>,
        response: Response,
        track_last_event_id: bool,
        initialize_request_context: bool,
        deliver_inbound: bool,
        generation: u64,
    ) -> Result<(), GuestError> {
        let mut stream = response.into_body().into_string_data_event_stream();

        while let Some(event) = stream.next().await {
            let event = event.map_err(http_error)?;
            {
                match self.current_lifecycle(generation) {
                    Ok(mut lifecycle) => {
                        if let Some(retry) = event.retry()
                            && let Some(stream) = lifecycle.stream()
                        {
                            stream.reconnect_delay = retry;
                        }
                        if track_last_event_id && let Some(data) = lifecycle.data_mut() {
                            data.last_event_id = event.id().map(ToOwned::to_owned);
                        }
                    }
                    Err(GuestError::SessionExpired)
                        if !track_last_event_id && !initialize_request_context => {}
                    Err(error) => return Err(error),
                }
            }
            let Some(data) = event.into_data() else {
                continue;
            };
            if data.trim().is_empty() {
                continue;
            }
            let message: JsonRpcMessage = serde_json::from_str(&data)?;
            self.inspect_inbound_message(&message, initialize_request_context, generation)
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
        let shutdown = self.shutdown_notify.notified();
        tokio::pin!(shutdown);
        shutdown.as_mut().enable();
        let (attempt, base_delay) = {
            let mut lifecycle = self.lifecycle();
            if !lifecycle.sse(HttpSseEvent::Retry) {
                return false;
            }
            let Some(stream) = lifecycle.stream() else {
                return false;
            };
            stream.reconnect_attempt = stream.reconnect_attempt.saturating_add(1);
            (
                stream.reconnect_attempt.min(u32::from(u8::MAX)) as u8,
                stream.reconnect_delay,
            )
        };
        let backoff = ExponentialBackoff::new()
            .max_attempts(u8::MAX)
            .base_delay_ms(base_delay.as_millis() as u64)
            .multiplier(2.0)
            .max_delay_ms(30_000);
        let mut rng: rand::rngs::StdRng = rand::make_rng();
        let delay_ms = backoff
            .delay(attempt, &mut rng)
            .unwrap_or(base_delay.as_millis() as u64);
        let delay = Duration::from_millis(delay_ms);
        tokio::select! {
            _ = tokio::time::sleep(delay) => true,
            _ = shutdown => false,
        }
    }

    fn capture_session_id(
        &self,
        headers: &rama::http::HeaderMap,
        generation: u64,
    ) -> Result<(), GuestError> {
        let mut lifecycle = self.current_lifecycle(generation)?;
        if let Some(session_id) = header_value(headers, HEADER_SESSION_ID)
            && let Some(data) = lifecycle.data_mut()
        {
            data.session_id = Some(session_id.to_string());
        }
        Ok(())
    }

    fn clear_session_state(&self, generation: u64) -> Result<(), GuestError> {
        if let Some(data) = self.current_lifecycle(generation)?.data_mut() {
            data.clear();
        }
        Ok(())
    }

    async fn recover_session(self: &Arc<Self>, observed_generation: u64) -> Result<(), GuestError> {
        let _recovery_guard = self.recovery_lock.lock().await;

        if self.closed.load(Ordering::Relaxed) {
            return Err(GuestError::Disconnected);
        }

        if self.session_generation.load(Ordering::Acquire) != observed_generation {
            return Ok(());
        }

        let initialize_message = self
            .lifecycle()
            .data()
            .and_then(|data| data.cached_initialize.clone())
            .ok_or(GuestError::SessionExpired)?;
        let initialized_sent = {
            let mut lifecycle = self.lifecycle();
            if self.closed.load(Ordering::Acquire) {
                return Err(GuestError::Disconnected);
            }
            if !lifecycle.session(HttpSessionEvent::Recover) {
                return Err(GuestError::Disconnected);
            }
            self.session_generation.fetch_add(1, Ordering::AcqRel);
            lifecycle
                .data_mut()
                .ok_or(GuestError::Disconnected)?
                .clear();
            lifecycle.initialized()
        };

        self.stop_sse_task().await;

        self.send_message_once(&initialize_message, false).await?;
        self.wait_for_session_ready(Duration::from_secs(10)).await?;

        if initialized_sent {
            let initialized = JsonRpcMessage::Notification(JsonRpcRequest::notification(
                "notifications/initialized",
                None,
            ));
            self.send_message_once(&initialized, true).await?;
            if !self.lifecycle().session(HttpSessionEvent::Initialized) {
                return Err(GuestError::Disconnected);
            }
        }

        {
            let mut lifecycle = self.lifecycle();
            if !lifecycle.session(HttpSessionEvent::Restored) {
                return Err(GuestError::Disconnected);
            }
            self.session_generation.fetch_add(1, Ordering::AcqRel);
        }
        if initialized_sent {
            self.ensure_sse_task().await;
        }
        Ok(())
    }

    async fn wait_for_session_ready(&self, timeout: Duration) -> Result<(), GuestError> {
        tokio::time::timeout(timeout, async {
            loop {
                let ready = self.initialize_ready.notified();
                tokio::pin!(ready);
                ready.as_mut().enable();
                if self.has_active_session().await {
                    break;
                }
                if self.closed.load(Ordering::Acquire) {
                    return Err(GuestError::Disconnected);
                }
                ready.await;
            }
            Ok(())
        })
        .await
        .map_err(|_| GuestError::Timeout(timeout))??;

        Ok(())
    }

    async fn has_active_session(&self) -> bool {
        self.lifecycle()
            .data()
            .is_some_and(|data| data.session_id.is_some() && data.negotiated_version.is_some())
    }

    async fn stop_sse_task(&self) {
        let handle = {
            let mut lifecycle = self.lifecycle();
            let handle = lifecycle.stream().and_then(|stream| stream.task.take());
            lifecycle.sse(HttpSseEvent::Stop);
            handle
        };
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn close_tasks(&self) {
        let (task, mut streams) = {
            let mut lifecycle = self.lifecycle();
            let task = lifecycle.stream().and_then(|stream| stream.task.take());
            let streams = lifecycle
                .data_mut()
                .map(|data| std::mem::take(&mut data.streams));
            lifecycle.close();
            (task, streams)
        };
        self.shutdown_notify.notify_waiters();
        self.initialize_ready.notify_waiters();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        if let Some(streams) = streams.as_mut() {
            streams.shutdown().await;
        }
    }

    async fn shutdown_inner(self: Arc<Self>) -> Result<(), GuestError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let (session_id, protocol_version) = {
            let lifecycle = self.lifecycle();
            lifecycle
                .data()
                .map(|data| (data.session_id.clone(), data.negotiated_version.clone()))
                .unwrap_or_default()
        };

        self.close_tasks().await;

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

    async fn force_shutdown_inner(&self) -> Result<(), GuestError> {
        self.closed.store(true, Ordering::SeqCst);
        self.close_tasks().await;
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
            let shutdown = inner.shutdown_notify.notified();
            tokio::pin!(shutdown);
            shutdown.as_mut().enable();
            if inner.closed.load(Ordering::Relaxed) {
                return Err(GuestError::Disconnected);
            }

            let mut receiver = inner.inbound_rx.lock().await;
            tokio::select! {
                message = receiver.recv() => {
                    message.ok_or(GuestError::Disconnected)
                }
                _ = shutdown => Err(GuestError::Disconnected),
            }
        })
    }

    fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.shutdown_inner().await })
    }

    fn force_shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.force_shutdown_inner().await })
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

fn http_error(error: impl std::fmt::Display) -> GuestError {
    GuestError::Http(error.to_string())
}

fn header_value(headers: &rama::http::HeaderMap, name: impl AsRef<str>) -> Option<&str> {
    headers
        .get(name.as_ref())
        .and_then(|value| value.to_str().ok())
}

async fn collect_body_bytes(response: Response) -> Result<Vec<u8>, GuestError> {
    let collected = response.into_body().collect().await.map_err(http_error)?;
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

/// Returns true only for the protocol-level rejection emitted when a request
/// reaches a stateful MCP endpoint without a session header.
///
/// The server rejects this before dispatching the JSON-RPC message, so it is
/// safe for `send_message_locked` to establish a fresh session and retry once,
/// including for non-idempotent methods such as `tools/call`. Generic 400
/// responses remain ordinary errors and are never replayed.
fn is_stale_session_rejection(status: StatusCode, body: Option<&str>) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }

    let Some(body) = body.map(str::trim) else {
        return false;
    };
    if body.eq_ignore_ascii_case("Missing Mcp-Session-Id header") {
        return true;
    }

    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .is_some_and(|value| {
            value
                .get("error")
                .and_then(|error| error.as_str())
                .is_some_and(|error| error.eq_ignore_ascii_case("Missing Mcp-Session-Id header"))
        })
}

#[cfg(test)]
mod tests;
