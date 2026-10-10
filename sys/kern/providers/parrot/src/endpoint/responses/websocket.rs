//! Session-owned Responses WebSocket v2 transport.
//!
//! Prewarming only performs an upgrade, never inference or credential refresh.
//! Requests carry full history: socket reuse does not imply server-side history
//! continuation. A changed endpoint or handshake header drops the old socket.
use std::sync::Arc;
use std::sync::OnceLock;

use futures::{SinkExt, StreamExt};
use jiff::SignedDuration;
use rama::http::HeaderMap;
use rama::http::StatusCode;
use rama::http::ws::handshake::client::{
    ClientWebSocket, HandshakeError, ResponseValidateError, WebSocketRequestBuilder,
};
use rama::http::ws::protocol::{Message, WebSocketConfig};
use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;
use tokio_util::task::AbortOnDropHandle;

use crate::common::{
    ResponseCreateWsRequestRef, ResponseEvent, ResponseStream, ResponsesApiRequest,
};
use crate::error::ApiError;
use crate::rate_limits::{parse_all_rate_limits, parse_rate_limit_event};
use crate::sse::responses::{ResponsesStreamEvent, process_responses_event};

const CONNECT_TIMEOUT: SignedDuration = SignedDuration::from_secs(5);
const WARM_TTL: SignedDuration = SignedDuration::from_secs(30);
const MAX_RESPONSE_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const RESPONSE_EVENT_BUFFER_CAPACITY: usize = 1600;
const WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";

pub(super) struct Connection {
    pub url: String,
    pub headers: HeaderMap,
    pub idle_timeout: SignedDuration,
    pub telemetry: Option<Arc<dyn chaos_client::RequestTelemetry>>,
}

impl Connection {
    fn key(&self) -> (String, Vec<(String, Vec<u8>)>) {
        let mut headers: Vec<_> = self
            .headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
            .collect();
        headers.sort();
        (self.url.clone(), headers)
    }
}

type Key = (String, Vec<(String, Vec<u8>)>);

enum Slot {
    Empty,
    Warming {
        key: Key,
        task: AbortOnDropHandle<Result<ClientWebSocket, ApiError>>,
        started: Instant,
    },
    Ready {
        key: Key,
        socket: Box<ClientWebSocket>,
        touched: Instant,
        /// Whether this socket was initialized with native web search.
        web_search_initialized: bool,
    },
}

/// One reusable transport per model session; never shared across credentials.
pub struct ResponsesWebSocket {
    slot: Arc<Mutex<Slot>>,
}

impl Default for ResponsesWebSocket {
    fn default() -> Self {
        Self {
            slot: Arc::new(Mutex::new(Slot::Empty)),
        }
    }
}

impl std::fmt::Debug for ResponsesWebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponsesWebSocket").finish_non_exhaustive()
    }
}

impl ResponsesWebSocket {
    pub async fn reset(&self) {
        *self.slot.lock().await = Slot::Empty;
    }

    pub(super) fn prewarm(&self, connection: Connection) {
        // Never wait for an active generation or stall session startup.
        let Ok(mut slot) = self.slot.try_lock() else {
            return;
        };
        let key = connection.key();
        match &*slot {
            Slot::Ready {
                key: current,
                touched,
                ..
            } if *current == key && elapsed_since(*touched) < WARM_TTL => return,
            Slot::Warming {
                key: current,
                started,
                ..
            } if *current == key && elapsed_since(*started) < WARM_TTL => return,
            _ => {}
        }
        *slot = Slot::Warming {
            key,
            task: AbortOnDropHandle::new(tokio::spawn(connect(connection))),
            started: Instant::now(),
        };
    }

    /// V2 only. Upgrade, send and stream failures never downgrade to HTTP.
    pub(super) async fn stream(
        &self,
        request: &ResponsesApiRequest,
        connection: Connection,
        turn_state: Option<Arc<OnceLock<String>>>,
        reconnect_on_web_search_activation: bool,
    ) -> Result<ResponseStream, ApiError> {
        let wire = serde_json::to_string(&ResponseCreateWsRequestRef::from(request))
            .map_err(|_| ApiError::Stream("failed to encode websocket request".into()))?;
        let key = connection.key();
        let has_web_search = request
            .tools
            .iter()
            .any(|tool| tool["type"] == "web_search");
        // Tokio's timer boundary requires an unsigned std duration. All
        // policy intervals and elapsed calculations use Jiff.
        let idle_timeout = connection.idle_timeout.unsigned_abs();
        let mut slot = self.slot.clone().lock_owned().await;
        let previous = std::mem::replace(&mut *slot, Slot::Empty);
        let (socket, source, web_search_initialized) = match previous {
            Slot::Ready {
                key: current,
                socket,
                touched,
                web_search_initialized,
            } if current == key
                && elapsed_since(touched) < WARM_TTL
                && !(reconnect_on_web_search_activation
                    && has_web_search
                    && !web_search_initialized) =>
            {
                (*socket, "reuse", web_search_initialized)
            }
            Slot::Warming {
                key: current,
                mut task,
                started,
            } if current == key && elapsed_since(started) < WARM_TTL => {
                let socket = (&mut task)
                    .await
                    .map_err(|_| ApiError::Stream("websocket prewarm was interrupted".into()))??;
                (socket, "prewarm", has_web_search)
            }
            old => {
                drop(old); // Abort mismatched prewarming; do not wait for it.
                (connect(connection).await?, "cold", has_web_search)
            }
        };
        let mut socket = socket;
        tracing::debug!(target: "chaos_parrot::websocket", source, "responses websocket request");
        // A canceled/failed send drops this socket. It may have reached the
        // server, so falling back here would risk duplicate generations.
        tokio::time::timeout(idle_timeout, socket.send(Message::text(wire)))
            .await
            .map_err(|_| ApiError::Stream("websocket send timed out".into()))?
            .map_err(|_| {
                ApiError::Stream("websocket send failed; request may have been accepted".into())
            })?;

        let headers = socket.response.headers.clone();
        if let Some(state) = &turn_state
            && let Some(value) = headers
                .get("x-codex-turn-state")
                .and_then(|v| v.to_str().ok())
        {
            let _ = state.set(value.to_owned());
        }
        let (tx, rx_event) = mpsc::channel(RESPONSE_EVENT_BUFFER_CAPACITY);
        tokio::spawn(async move {
            for event in handshake_events(&headers) {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
            loop {
                let message = tokio::select! {
                    _ = tx.closed() => return,
                    message = tokio::time::timeout(idle_timeout, socket.next()) => message,
                };
                let message = match message {
                    Ok(Some(Ok(message))) => message,
                    _ => {
                        let _ = tx
                            .send(Err(ApiError::Stream(
                                "websocket closed or timed out before response.completed".into(),
                            )))
                            .await;
                        return;
                    }
                };
                let text = match message {
                    Message::Text(text) => text,
                    Message::Ping(_) | Message::Pong(_) => continue,
                    _ => {
                        let _ = tx
                            .send(Err(ApiError::Stream(
                                "unexpected websocket message before response.completed".into(),
                            )))
                            .await;
                        return;
                    }
                };
                let value: serde_json::Value = match serde_json::from_str(text.as_str()) {
                    Ok(value) => value,
                    Err(_) => {
                        let _ = tx
                            .send(Err(ApiError::Stream("invalid websocket JSON event".into())))
                            .await;
                        return;
                    }
                };
                if value["type"] == "error" {
                    let status = value
                        .get("status")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|v| u16::try_from(v).ok())
                        .and_then(|v| StatusCode::from_u16(v).ok())
                        .unwrap_or(StatusCode::BAD_GATEWAY);
                    let message = value["error"]["message"]
                        .as_str()
                        .unwrap_or("websocket API error")
                        .to_owned();
                    let _ = tx.send(Err(ApiError::Api { status, message })).await;
                    return;
                }
                if let Some(snapshot) = parse_rate_limit_event(text.as_str())
                    && tx
                        .send(Ok(ResponseEvent::RateLimits(snapshot)))
                        .await
                        .is_err()
                {
                    return;
                }
                let event: ResponsesStreamEvent = match serde_json::from_value(value) {
                    Ok(event) => event,
                    Err(_) => {
                        let _ = tx
                            .send(Err(ApiError::Stream(
                                "malformed websocket response event".into(),
                            )))
                            .await;
                        return;
                    }
                };
                if let Some(model) = event.response_model()
                    && tx
                        .send(Ok(ResponseEvent::ServerModel(model)))
                        .await
                        .is_err()
                {
                    return;
                }
                match process_responses_event(event) {
                    Ok(Some(event)) => {
                        if matches!(event, ResponseEvent::Completed { .. }) {
                            // Publish the socket before Completed unlocks the next
                            // tool round. No speculative history is retained.
                            *slot = Slot::Ready {
                                key,
                                socket: Box::new(socket),
                                touched: Instant::now(),
                                web_search_initialized,
                            };
                            drop(slot);
                            let _ = tx.send(Ok(event)).await;
                            return;
                        }
                        if tx.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = tx.send(Err(error.into_api_error())).await;
                        return;
                    }
                }
            }
        });
        Ok(ResponseStream { rx_event })
    }
}

async fn connect(connection: Connection) -> Result<ClientWebSocket, ApiError> {
    let started = Instant::now();
    // Same TLS, proxy and infrastructure-cookie stack as the HTTP adapter.
    let client = chaos_client::default_rama_http_client();
    let mut builder = WebSocketRequestBuilder::new_with_service(&client, connection.url)
        .with_config(WebSocketConfig::default().with_max_message_size(MAX_RESPONSE_MESSAGE_BYTES));
    for (name, value) in &connection.headers {
        builder = builder.with_header(name.clone(), value.clone());
    }
    let beta = connection
        .headers
        .get("openai-beta")
        .and_then(|v| v.to_str().ok())
        .map(|existing| format!("{existing},{WEBSOCKET_BETA}"))
        .unwrap_or_else(|| WEBSOCKET_BETA.to_owned());
    let result = tokio::time::timeout(
        CONNECT_TIMEOUT.unsigned_abs(),
        builder
            .with_header_overwrite("openai-beta", beta)
            .handshake(Default::default()),
    )
    .await;
    let result = match result {
        Ok(Ok(socket)) => Ok(socket),
        Ok(Err(HandshakeError::ValidationError(ResponseValidateError::UnexpectedStatusCode(
            status,
        )))) => Err(ApiError::Api {
            status,
            message: "responses websocket v2 upgrade rejected".into(),
        }),
        Ok(Err(_)) => Err(ApiError::Transport(chaos_client::TransportError::Network(
            "responses websocket v2 upgrade failed".into(),
        ))),
        Err(_) => Err(ApiError::Transport(chaos_client::TransportError::Network(
            "responses websocket v2 upgrade timed out".into(),
        ))),
    };
    if let Some(telemetry) = connection.telemetry {
        let status = match &result {
            Ok(_) => Some(StatusCode::SWITCHING_PROTOCOLS),
            Err(ApiError::Api { status, .. }) => Some(*status),
            Err(_) => None,
        };
        let error = result.as_ref().err().map(|_| {
            chaos_client::TransportError::Network("responses websocket v2 upgrade failed".into())
        });
        telemetry.on_request(1, status, error.as_ref(), started.elapsed());
    }
    tracing::debug!(target: "chaos_parrot::websocket", ready = result.is_ok(),
        duration_ms = elapsed_since(started).as_secs_f64() * 1000.0, "responses websocket upgrade");
    result
}

fn elapsed_since(start: Instant) -> SignedDuration {
    SignedDuration::try_from(start.elapsed()).unwrap_or(SignedDuration::MAX)
}

fn handshake_events(headers: &HeaderMap) -> Vec<ResponseEvent> {
    let mut events = Vec::new();
    if let Some(model) = headers.get("openai-model").and_then(|v| v.to_str().ok()) {
        events.push(ResponseEvent::ServerModel(model.into()));
    }
    events.extend(
        parse_all_rate_limits(headers, true)
            .into_iter()
            .map(ResponseEvent::RateLimits),
    );
    if let Some(etag) = headers.get("x-models-etag").and_then(|v| v.to_str().ok()) {
        events.push(ResponseEvent::ModelsEtag(etag.into()));
    }
    if headers.contains_key("x-reasoning-included") {
        events.push(ResponseEvent::ServerReasoningIncluded(true));
    }
    events
}

#[cfg(test)]
mod tests;
