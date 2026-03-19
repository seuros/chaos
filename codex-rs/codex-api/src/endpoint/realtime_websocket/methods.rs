use crate::endpoint::realtime_websocket::methods_common::conversation_handoff_append_message;
use crate::endpoint::realtime_websocket::methods_common::conversation_item_create_message;
use crate::endpoint::realtime_websocket::methods_common::normalized_session_mode;
use crate::endpoint::realtime_websocket::methods_common::session_update_session;
use crate::endpoint::realtime_websocket::methods_common::websocket_intent;
use crate::endpoint::realtime_websocket::protocol::RealtimeAudioFrame;
use crate::endpoint::realtime_websocket::protocol::RealtimeEvent;
use crate::endpoint::realtime_websocket::protocol::RealtimeEventParser;
use crate::endpoint::realtime_websocket::protocol::RealtimeOutboundMessage;
use crate::endpoint::realtime_websocket::protocol::RealtimeSessionConfig;
use crate::endpoint::realtime_websocket::protocol::RealtimeSessionMode;
use crate::endpoint::realtime_websocket::protocol::RealtimeTranscriptDelta;
use crate::endpoint::realtime_websocket::protocol::RealtimeTranscriptEntry;
use crate::endpoint::realtime_websocket::protocol::parse_realtime_event;
use crate::error::ApiError;
use crate::provider::Provider;
use codex_client::maybe_build_rustls_client_config_with_custom_ca;
use codex_utils_rustls_provider::ensure_rustls_crypto_provider;
use futures::SinkExt;
use futures::StreamExt;
use http::HeaderMap;
use http::HeaderValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use rama::error::BoxError;
use rama::extensions::Extensions;
use rama::http::client::EasyHttpConnectorBuilder;
use rama::http::ws::Message;
use rama::http::ws::handshake::client::ClientWebSocket;
use rama::http::ws::handshake::client::HttpClientWebSocketExt;
use rama::tls::rustls::client::TlsConnectorData;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::trace;
use url::Url;

struct WsStream {
    tx_command: mpsc::Sender<WsCommand>,
    pump_task: tokio::task::JoinHandle<()>,
}

enum WsCommand {
    Send {
        message: Message,
        tx_result: oneshot::Sender<Result<(), BoxError>>,
    },
    Close {
        tx_result: oneshot::Sender<Result<(), BoxError>>,
    },
}

impl WsStream {
    fn new(
        inner: ClientWebSocket,
    ) -> (Self, mpsc::UnboundedReceiver<Result<Message, BoxError>>) {
        let (tx_command, mut rx_command) = mpsc::channel::<WsCommand>(32);
        let (tx_message, rx_message) = mpsc::unbounded_channel::<Result<Message, BoxError>>();

        let pump_task = tokio::spawn(async move {
            let mut inner = inner;
            loop {
                tokio::select! {
                    command = rx_command.recv() => {
                        let Some(command) = command else {
                            break;
                        };
                        match command {
                            WsCommand::Send { message, tx_result } => {
                                debug!("realtime websocket sending message");
                                let result: Result<(), BoxError> = inner.send(message).await
                                    .map_err(|e| -> BoxError { Box::new(e) });
                                let should_break = result.is_err();
                                if let Err(err) = &result {
                                    error!("realtime websocket send failed: {err}");
                                }
                                let _ = tx_result.send(result);
                                if should_break {
                                    break;
                                }
                            }
                            WsCommand::Close { tx_result } => {
                                info!("realtime websocket sending close");
                                let result: Result<(), BoxError> = inner.close(None).await
                                    .map_err(|e| -> BoxError { Box::new(e) });
                                if let Err(err) = &result {
                                    error!("realtime websocket close failed: {err}");
                                }
                                let _ = tx_result.send(result);
                                break;
                            }
                        }
                    }
                    message = inner.next() => {
                        let Some(message) = message else {
                            break;
                        };
                        match message {
                            Ok(Message::Ping(payload)) => {
                                trace!(payload_len = payload.len(), "realtime websocket received ping");
                                if let Err(err) = inner.send(Message::Pong(payload)).await {
                                    error!("realtime websocket failed to send pong: {err}");
                                    let _ = tx_message.send(Err(Box::new(err) as BoxError));
                                    break;
                                }
                            }
                            Ok(Message::Pong(_)) => {}
                            Ok(message @ (Message::Text(_)
                                | Message::Binary(_)
                                | Message::Close(_)
                                | Message::Frame(_))) => {
                                let is_close = matches!(message, Message::Close(_));
                                match &message {
                                    Message::Text(_) => trace!("realtime websocket received text frame"),
                                    Message::Binary(binary) => {
                                        error!(
                                            payload_len = binary.len(),
                                            "realtime websocket received unexpected binary frame"
                                        );
                                    }
                                    Message::Close(frame) => info!(
                                        "realtime websocket received close frame: code={:?} reason={:?}",
                                        frame.as_ref().map(|frame| frame.code),
                                        frame.as_ref().map(|frame| frame.reason.as_str())
                                    ),
                                    Message::Frame(_) => {
                                        trace!("realtime websocket received raw frame");
                                    }
                                    Message::Ping(_) | Message::Pong(_) => {}
                                }
                                if tx_message.send(Ok(message)).is_err() {
                                    break;
                                }
                                if is_close {
                                    break;
                                }
                            }
                            Err(err) => {
                                error!("realtime websocket receive failed: {err}");
                                let _ = tx_message.send(Err(Box::new(err) as BoxError));
                                break;
                            }
                        }
                    }
                }
            }
            info!("realtime websocket pump exiting");
        });

        (
            Self {
                tx_command,
                pump_task,
            },
            rx_message,
        )
    }

    async fn request(
        &self,
        make_command: impl FnOnce(oneshot::Sender<Result<(), BoxError>>) -> WsCommand,
    ) -> Result<(), BoxError> {
        let (tx_result, rx_result) = oneshot::channel();
        if self.tx_command.send(make_command(tx_result)).await.is_err() {
            return Err("connection closed".into());
        }
        rx_result.await.unwrap_or_else(|_| Err("connection closed".into()))
    }

    async fn send(&self, message: Message) -> Result<(), BoxError> {
        self.request(|tx_result| WsCommand::Send { message, tx_result })
            .await
    }

    async fn close(&self) -> Result<(), BoxError> {
        self.request(|tx_result| WsCommand::Close { tx_result })
            .await
    }
}

impl Drop for WsStream {
    fn drop(&mut self) {
        self.pump_task.abort();
    }
}

pub struct RealtimeWebsocketConnection {
    writer: RealtimeWebsocketWriter,
    events: RealtimeWebsocketEvents,
}

#[derive(Clone)]
pub struct RealtimeWebsocketWriter {
    stream: Arc<WsStream>,
    is_closed: Arc<AtomicBool>,
    event_parser: RealtimeEventParser,
}

#[derive(Clone)]
pub struct RealtimeWebsocketEvents {
    rx_message: Arc<Mutex<mpsc::UnboundedReceiver<Result<Message, BoxError>>>>,
    active_transcript: Arc<Mutex<ActiveTranscriptState>>,
    event_parser: RealtimeEventParser,
    is_closed: Arc<AtomicBool>,
}

#[derive(Default)]
struct ActiveTranscriptState {
    entries: Vec<RealtimeTranscriptEntry>,
}

impl RealtimeWebsocketConnection {
    pub async fn send_audio_frame(&self, frame: RealtimeAudioFrame) -> Result<(), ApiError> {
        self.writer.send_audio_frame(frame).await
    }

    pub async fn send_conversation_item_create(&self, text: String) -> Result<(), ApiError> {
        self.writer.send_conversation_item_create(text).await
    }

    pub async fn send_conversation_handoff_append(
        &self,
        handoff_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.writer
            .send_conversation_handoff_append(handoff_id, output_text)
            .await
    }

    pub async fn close(&self) -> Result<(), ApiError> {
        self.writer.close().await
    }

    pub async fn next_event(&self) -> Result<Option<RealtimeEvent>, ApiError> {
        self.events.next_event().await
    }

    pub fn writer(&self) -> RealtimeWebsocketWriter {
        self.writer.clone()
    }

    pub fn events(&self) -> RealtimeWebsocketEvents {
        self.events.clone()
    }

    fn new(
        stream: WsStream,
        rx_message: mpsc::UnboundedReceiver<Result<Message, BoxError>>,
        event_parser: RealtimeEventParser,
    ) -> Self {
        let stream = Arc::new(stream);
        let is_closed = Arc::new(AtomicBool::new(false));
        Self {
            writer: RealtimeWebsocketWriter {
                stream: Arc::clone(&stream),
                is_closed: Arc::clone(&is_closed),
                event_parser,
            },
            events: RealtimeWebsocketEvents {
                rx_message: Arc::new(Mutex::new(rx_message)),
                active_transcript: Arc::new(Mutex::new(ActiveTranscriptState::default())),
                event_parser,
                is_closed,
            },
        }
    }
}

impl RealtimeWebsocketWriter {
    pub async fn send_audio_frame(&self, frame: RealtimeAudioFrame) -> Result<(), ApiError> {
        self.send_json(RealtimeOutboundMessage::InputAudioBufferAppend { audio: frame.data })
            .await
    }

    pub async fn send_conversation_item_create(&self, text: String) -> Result<(), ApiError> {
        self.send_json(conversation_item_create_message(self.event_parser, text))
            .await
    }

    pub async fn send_conversation_handoff_append(
        &self,
        handoff_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.send_json(conversation_handoff_append_message(
            self.event_parser,
            handoff_id,
            output_text,
        ))
        .await
    }

    pub async fn send_session_update(
        &self,
        instructions: String,
        session_mode: RealtimeSessionMode,
    ) -> Result<(), ApiError> {
        let session_mode = normalized_session_mode(self.event_parser, session_mode);
        let session = session_update_session(self.event_parser, instructions, session_mode);
        self.send_json(RealtimeOutboundMessage::SessionUpdate { session })
            .await
    }

    pub async fn close(&self) -> Result<(), ApiError> {
        if self.is_closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if let Err(err) = self.stream.close().await {
            debug!("realtime websocket close error (may already be closed): {err}");
        }
        Ok(())
    }

    async fn send_json(&self, message: RealtimeOutboundMessage) -> Result<(), ApiError> {
        let payload = serde_json::to_string(&message)
            .map_err(|err| ApiError::Stream(format!("failed to encode realtime request: {err}")))?;
        debug!(?message, "realtime websocket request");

        if self.is_closed.load(Ordering::SeqCst) {
            return Err(ApiError::Stream(
                "realtime websocket connection is closed".to_string(),
            ));
        }

        self.stream
            .send(Message::text(payload))
            .await
            .map_err(|err| ApiError::Stream(format!("failed to send realtime request: {err}")))?;
        Ok(())
    }
}

impl RealtimeWebsocketEvents {
    pub async fn next_event(&self) -> Result<Option<RealtimeEvent>, ApiError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Ok(None);
        }

        loop {
            let msg = match self.rx_message.lock().await.recv().await {
                Some(Ok(msg)) => msg,
                Some(Err(err)) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    error!("realtime websocket read failed: {err}");
                    return Err(ApiError::Stream(format!(
                        "failed to read websocket message: {err}"
                    )));
                }
                None => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    info!("realtime websocket event stream ended");
                    return Ok(None);
                }
            };

            match msg {
                Message::Text(text) => {
                    if let Some(mut event) = parse_realtime_event(&text, self.event_parser) {
                        self.update_active_transcript(&mut event).await;
                        debug!(?event, "realtime websocket parsed event");
                        return Ok(Some(event));
                    }
                    debug!("realtime websocket ignored unsupported text frame");
                }
                Message::Close(frame) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    info!(
                        "realtime websocket closed: code={:?} reason={:?}",
                        frame.as_ref().map(|frame| frame.code),
                        frame.as_ref().map(|frame| frame.reason.as_str())
                    );
                    return Ok(None);
                }
                Message::Binary(_) => {
                    return Ok(Some(RealtimeEvent::Error(
                        "unexpected binary realtime websocket event".to_string(),
                    )));
                }
                Message::Frame(_) | Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    }

    async fn update_active_transcript(&self, event: &mut RealtimeEvent) {
        let mut active_transcript = self.active_transcript.lock().await;
        match event {
            RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta { delta }) => {
                append_transcript_delta(&mut active_transcript.entries, "user", delta);
            }
            RealtimeEvent::OutputTranscriptDelta(RealtimeTranscriptDelta { delta }) => {
                append_transcript_delta(&mut active_transcript.entries, "assistant", delta);
            }
            RealtimeEvent::HandoffRequested(handoff) => {
                handoff.active_transcript = std::mem::take(&mut active_transcript.entries);
            }
            RealtimeEvent::SessionUpdated { .. }
            | RealtimeEvent::AudioOut(_)
            | RealtimeEvent::ConversationItemAdded(_)
            | RealtimeEvent::ConversationItemDone { .. }
            | RealtimeEvent::Error(_) => {}
        }
    }
}

fn append_transcript_delta(entries: &mut Vec<RealtimeTranscriptEntry>, role: &str, delta: &str) {
    if delta.is_empty() {
        return;
    }

    if let Some(last_entry) = entries.last_mut()
        && last_entry.role == role
    {
        last_entry.text.push_str(delta);
        return;
    }

    entries.push(RealtimeTranscriptEntry {
        role: role.to_string(),
        text: delta.to_string(),
    });
}

pub struct RealtimeWebsocketClient {
    provider: Provider,
}

impl RealtimeWebsocketClient {
    pub fn new(provider: Provider) -> Self {
        Self { provider }
    }

    pub async fn connect(
        &self,
        config: RealtimeSessionConfig,
        extra_headers: HeaderMap,
        default_headers: HeaderMap,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        ensure_rustls_crypto_provider();
        let ws_url = websocket_url_from_api_url(
            self.provider.base_url.as_str(),
            self.provider.query_params.as_ref(),
            config.model.as_deref(),
            config.event_parser,
            config.session_mode,
        )?;

        let headers = merge_request_headers(
            &self.provider.headers,
            with_session_id_header(extra_headers, config.session_id.as_deref())?,
            default_headers,
        );

        info!("connecting realtime websocket: {ws_url}");
        let tls_data = match maybe_build_rustls_client_config_with_custom_ca()
            .map_err(|err| ApiError::Stream(format!("failed to configure websocket TLS: {err}")))?
        {
            Some(config) => TlsConnectorData::from(config),
            None => TlsConnectorData::try_new_http_auto()
                .map_err(|err| ApiError::Stream(format!("failed to configure websocket TLS: {err}")))?,
        };

        let client = EasyHttpConnectorBuilder::new()
            .with_default_transport_connector()
            .without_tls_proxy_support()
            .without_proxy_support()
            .with_tls_support_using_rustls(Some(tls_data))
            .with_default_http_connector(Default::default())
            .build_client();

        let mut ws_builder = client.websocket(ws_url.as_str());
        for (name, value) in &headers {
            ws_builder = ws_builder.with_header(name.as_str(), value.to_str().map_err(|err| {
                ApiError::Stream(format!("invalid header value for {name}: {err}"))
            })?);
        }

        let ws = ws_builder
            .handshake(Extensions::default())
            .await
            .map_err(|err| ApiError::Stream(format!("failed to connect realtime websocket: {err}")))?;

        let response_parts = ws.response();
        info!(
            ws_url = %ws_url,
            status = %response_parts.status,
            "realtime websocket connected"
        );

        let (stream, rx_message) = WsStream::new(ws);
        let connection = RealtimeWebsocketConnection::new(stream, rx_message, config.event_parser);
        debug!(
            session_id = config.session_id.as_deref().unwrap_or("<none>"),
            "realtime websocket sending session.update"
        );
        connection
            .writer
            .send_session_update(config.instructions, config.session_mode)
            .await?;
        Ok(connection)
    }
}

fn merge_request_headers(
    provider_headers: &HeaderMap,
    extra_headers: HeaderMap,
    default_headers: HeaderMap,
) -> HeaderMap {
    let mut headers = provider_headers.clone();
    headers.extend(extra_headers);
    for (name, value) in &default_headers {
        if let http::header::Entry::Vacant(entry) = headers.entry(name) {
            entry.insert(value.clone());
        }
    }
    headers
}

fn with_session_id_header(
    mut headers: HeaderMap,
    session_id: Option<&str>,
) -> Result<HeaderMap, ApiError> {
    let Some(session_id) = session_id else {
        return Ok(headers);
    };
    headers.insert(
        "x-session-id",
        HeaderValue::from_str(session_id).map_err(|err| {
            ApiError::Stream(format!("invalid realtime session id header: {err}"))
        })?,
    );
    Ok(headers)
}

fn websocket_url_from_api_url(
    api_url: &str,
    query_params: Option<&HashMap<String, String>>,
    model: Option<&str>,
    event_parser: RealtimeEventParser,
    _session_mode: RealtimeSessionMode,
) -> Result<Url, ApiError> {
    let mut url = Url::parse(api_url)
        .map_err(|err| ApiError::Stream(format!("failed to parse realtime api_url: {err}")))?;

    normalize_realtime_path(&mut url);

    match url.scheme() {
        "ws" | "wss" => {}
        "http" | "https" => {
            let scheme = if url.scheme() == "http" { "ws" } else { "wss" };
            let _ = url.set_scheme(scheme);
        }
        scheme => {
            return Err(ApiError::Stream(format!(
                "unsupported realtime api_url scheme: {scheme}"
            )));
        }
    }

    let intent = websocket_intent(event_parser);
    let has_extra_query_params = query_params.is_some_and(|query_params| {
        query_params
            .iter()
            .any(|(key, _)| key != "intent" && !(key == "model" && model.is_some()))
    });
    if intent.is_some() || model.is_some() || has_extra_query_params {
        let mut query = url.query_pairs_mut();
        if let Some(intent) = intent {
            query.append_pair("intent", intent);
        }
        if let Some(model) = model {
            query.append_pair("model", model);
        }
        if let Some(query_params) = query_params {
            for (key, value) in query_params {
                if key == "intent" || (key == "model" && model.is_some()) {
                    continue;
                }
                query.append_pair(key, value);
            }
        }
    }

    Ok(url)
}

fn normalize_realtime_path(url: &mut Url) {
    let path = url.path().to_string();
    if path.is_empty() || path == "/" {
        url.set_path("/v1/realtime");
        return;
    }

    if path.ends_with("/realtime") {
        return;
    }

    if path.ends_with("/realtime/") {
        url.set_path(path.trim_end_matches('/'));
        return;
    }

    if path.ends_with("/v1") {
        url.set_path(&format!("{path}/realtime"));
        return;
    }

    if path.ends_with("/v1/") {
        url.set_path(&format!("{path}realtime"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::realtime_websocket::protocol::RealtimeHandoffRequested;
    use crate::endpoint::realtime_websocket::protocol::RealtimeTranscriptDelta;
    use crate::endpoint::realtime_websocket::protocol::RealtimeTranscriptEntry;
    use http::HeaderValue;
    use pretty_assertions::assert_eq;
    use serde_json::Value;
    use serde_json::json;
    use std::collections::HashMap;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use rama::http::ws::Message as WsMessage;

    async fn accept_ws(stream: tokio::net::TcpStream) -> rama::http::ws::AsyncWebSocket<rama::tcp::TcpStream> {
        use base64::Engine;
        use sha1::{Digest, Sha1};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let mut reader = BufReader::new(stream);
        let mut ws_key = None;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read header line");
            if line == "\r\n" || line == "\n" { break; }
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("sec-websocket-key") {
                    ws_key = Some(value.trim().trim_end_matches(['\r', '\n']).to_string());
                }
            }
        }
        let ws_key = ws_key.expect("missing Sec-WebSocket-Key");
        let mut hasher = Sha1::new();
        hasher.update(ws_key.as_bytes());
        hasher.update(b"258EAFA5-E914-47DA-95CA-5ADF5B3F4A84");
        let accept = base64::engine::general_purpose::STANDARD.encode(hasher.finalize());
        let remaining = reader.buffer().to_vec();
        let mut stream = reader.into_inner();
        stream.write_all(format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        ).as_bytes()).await.expect("write 101");
        let stream = rama::tcp::TcpStream::new(stream);
        if remaining.is_empty() {
            rama::http::ws::AsyncWebSocket::from_raw_socket(stream, rama::http::ws::protocol::Role::Server, None).await
        } else {
            rama::http::ws::AsyncWebSocket::from_partially_read(stream, remaining, rama::http::ws::protocol::Role::Server, None).await
        }
    }

    #[test]
    fn parse_session_updated_event() {
        let payload = json!({
            "type": "session.updated",
            "session": {"id": "sess_123", "instructions": "backend prompt"}
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::SessionUpdated {
                session_id: "sess_123".to_string(),
                instructions: Some("backend prompt".to_string()),
            })
        );
    }

    #[test]
    fn parse_audio_delta_event() {
        let payload = json!({
            "type": "conversation.output_audio.delta",
            "delta": "AAA=",
            "sample_rate": 48000,
            "channels": 1,
            "samples_per_channel": 960
        })
        .to_string();
        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::AudioOut(RealtimeAudioFrame {
                data: "AAA=".to_string(),
                sample_rate: 48000,
                num_channels: 1,
                samples_per_channel: Some(960),
            }))
        );
    }

    #[test]
    fn parse_conversation_item_added_event() {
        let payload = json!({
            "type": "conversation.item.added",
            "item": {"type": "message", "seq": 7}
        })
        .to_string();
        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::ConversationItemAdded(
                json!({"type": "message", "seq": 7})
            ))
        );
    }

    #[test]
    fn parse_conversation_item_done_event() {
        let payload = json!({
            "type": "conversation.item.done",
            "item": {"id": "item_123", "type": "message"}
        })
        .to_string();
        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::ConversationItemDone {
                item_id: "item_123".to_string(),
            })
        );
    }

    #[test]
    fn parse_handoff_requested_event() {
        let payload = json!({
            "type": "conversation.handoff.requested",
            "handoff_id": "handoff_123",
            "item_id": "item_123",
            "input_transcript": "delegate this"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
                handoff_id: "handoff_123".to_string(),
                item_id: "item_123".to_string(),
                input_transcript: "delegate this".to_string(),
                active_transcript: Vec::new(),
            }))
        );
    }

    #[test]
    fn parse_input_transcript_delta_event() {
        let payload = json!({
            "type": "conversation.input_transcript.delta",
            "delta": "hello "
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::InputTranscriptDelta(
                RealtimeTranscriptDelta {
                    delta: "hello ".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_output_transcript_delta_event() {
        let payload = json!({
            "type": "conversation.output_transcript.delta",
            "delta": "hi"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
            Some(RealtimeEvent::OutputTranscriptDelta(
                RealtimeTranscriptDelta {
                    delta: "hi".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_realtime_v2_handoff_tool_call_event() {
        let payload = json!({
            "type": "conversation.item.done",
            "item": {
                "id": "item_123",
                "type": "function_call",
                "name": "codex",
                "call_id": "call_123",
                "arguments": "{\"prompt\":\"delegate this\"}"
            }
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
                handoff_id: "call_123".to_string(),
                item_id: "item_123".to_string(),
                input_transcript: "delegate this".to_string(),
                active_transcript: Vec::new(),
            }))
        );
    }

    #[test]
    fn parse_realtime_v2_input_audio_transcription_delta_event() {
        let payload = json!({
            "type": "conversation.item.input_audio_transcription.delta",
            "delta": "hello"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::InputTranscriptDelta(
                RealtimeTranscriptDelta {
                    delta: "hello".to_string(),
                }
            ))
        );
    }

    #[test]
    fn parse_realtime_v2_output_audio_delta_defaults_audio_shape() {
        let payload = json!({
            "type": "response.output_audio.delta",
            "delta": "AQID"
        })
        .to_string();

        assert_eq!(
            parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
            Some(RealtimeEvent::AudioOut(RealtimeAudioFrame {
                data: "AQID".to_string(),
                sample_rate: 24_000,
                num_channels: 1,
                samples_per_channel: None,
            }))
        );
    }

    #[test]
    fn merge_request_headers_matches_http_precedence() {
        let mut provider_headers = HeaderMap::new();
        provider_headers.insert(
            "originator",
            HeaderValue::from_static("provider-originator"),
        );
        provider_headers.insert("x-priority", HeaderValue::from_static("provider"));

        let mut extra_headers = HeaderMap::new();
        extra_headers.insert("x-priority", HeaderValue::from_static("extra"));

        let mut default_headers = HeaderMap::new();
        default_headers.insert("originator", HeaderValue::from_static("default-originator"));
        default_headers.insert("x-priority", HeaderValue::from_static("default"));
        default_headers.insert("x-default-only", HeaderValue::from_static("default-only"));

        let merged = merge_request_headers(&provider_headers, extra_headers, default_headers);

        assert_eq!(
            merged.get("originator"),
            Some(&HeaderValue::from_static("provider-originator"))
        );
        assert_eq!(
            merged.get("x-priority"),
            Some(&HeaderValue::from_static("extra"))
        );
        assert_eq!(
            merged.get("x-default-only"),
            Some(&HeaderValue::from_static("default-only"))
        );
    }

    #[test]
    fn websocket_url_from_http_base_defaults_to_ws_path() {
        let url = websocket_url_from_api_url(
            "http://127.0.0.1:8011",
            None,
            None,
            RealtimeEventParser::V1,
            RealtimeSessionMode::Conversational,
        )
        .expect("build ws url");
        assert_eq!(
            url.as_str(),
            "ws://127.0.0.1:8011/v1/realtime?intent=quicksilver"
        );
    }

    #[test]
    fn websocket_url_from_ws_base_defaults_to_ws_path() {
        let url = websocket_url_from_api_url(
            "wss://example.com",
            None,
            Some("realtime-test-model"),
            RealtimeEventParser::V1,
            RealtimeSessionMode::Conversational,
        )
        .expect("build ws url");
        assert_eq!(
            url.as_str(),
            "wss://example.com/v1/realtime?intent=quicksilver&model=realtime-test-model"
        );
    }

    #[test]
    fn websocket_url_from_v1_base_appends_realtime_path() {
        let url = websocket_url_from_api_url(
            "https://api.openai.com/v1",
            None,
            Some("snapshot"),
            RealtimeEventParser::V1,
            RealtimeSessionMode::Conversational,
        )
        .expect("build ws url");
        assert_eq!(
            url.as_str(),
            "wss://api.openai.com/v1/realtime?intent=quicksilver&model=snapshot"
        );
    }

    #[test]
    fn websocket_url_from_nested_v1_base_appends_realtime_path() {
        let url = websocket_url_from_api_url(
            "https://example.com/openai/v1",
            None,
            Some("snapshot"),
            RealtimeEventParser::V1,
            RealtimeSessionMode::Conversational,
        )
        .expect("build ws url");
        assert_eq!(
            url.as_str(),
            "wss://example.com/openai/v1/realtime?intent=quicksilver&model=snapshot"
        );
    }

    #[test]
    fn websocket_url_preserves_existing_realtime_path_and_extra_query_params() {
        let url = websocket_url_from_api_url(
            "https://example.com/v1/realtime?foo=bar",
            Some(&HashMap::from([
                ("trace".to_string(), "1".to_string()),
                ("intent".to_string(), "ignored".to_string()),
            ])),
            Some("snapshot"),
            RealtimeEventParser::V1,
            RealtimeSessionMode::Conversational,
        )
        .expect("build ws url");
        assert_eq!(
            url.as_str(),
            "wss://example.com/v1/realtime?foo=bar&intent=quicksilver&model=snapshot&trace=1"
        );
    }

    #[test]
    fn websocket_url_v1_ignores_transcription_mode() {
        let url = websocket_url_from_api_url(
            "https://example.com",
            None,
            None,
            RealtimeEventParser::V1,
            RealtimeSessionMode::Transcription,
        )
        .expect("build ws url");
        assert_eq!(
            url.as_str(),
            "wss://example.com/v1/realtime?intent=quicksilver"
        );
    }

    #[test]
    fn websocket_url_omits_intent_for_realtime_v2_conversational_mode() {
        let url = websocket_url_from_api_url(
            "https://example.com/v1/realtime?foo=bar",
            Some(&HashMap::from([
                ("trace".to_string(), "1".to_string()),
                ("intent".to_string(), "ignored".to_string()),
            ])),
            Some("snapshot"),
            RealtimeEventParser::RealtimeV2,
            RealtimeSessionMode::Conversational,
        )
        .expect("build ws url");
        assert_eq!(
            url.as_str(),
            "wss://example.com/v1/realtime?foo=bar&model=snapshot&trace=1"
        );
    }

    #[test]
    fn websocket_url_omits_intent_for_realtime_v2_transcription_mode() {
        let url = websocket_url_from_api_url(
            "https://example.com",
            None,
            None,
            RealtimeEventParser::RealtimeV2,
            RealtimeSessionMode::Transcription,
        )
        .expect("build ws url");
        assert_eq!(url.as_str(), "wss://example.com/v1/realtime");
    }

    #[tokio::test]
    async fn e2e_connect_and_exchange_events_against_mock_ws_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_ws(stream).await;

            let first = ws
                .recv_message()
                .await
                .expect("first msg")
                .into_text()
                .expect("text");
            let first_json: Value = serde_json::from_str(&first).expect("json");
            assert_eq!(first_json["type"], "session.update");
            assert_eq!(
                first_json["session"]["type"],
                Value::String("quicksilver".to_string())
            );
            assert_eq!(
                first_json["session"]["instructions"],
                Value::String("backend prompt".to_string())
            );
            assert_eq!(
                first_json["session"]["audio"]["input"]["format"]["type"],
                Value::String("audio/pcm".to_string())
            );
            assert_eq!(
                first_json["session"]["audio"]["input"]["format"]["rate"],
                Value::from(24_000)
            );
            assert_eq!(
                first_json["session"]["audio"]["output"]["voice"],
                Value::String("fathom".to_string())
            );

            ws.send_message(WsMessage::text(
                json!({
                    "type": "session.updated",
                    "session": {"id": "sess_mock", "instructions": "backend prompt"}
                })
                .to_string(),
            ))
            .await
            .expect("send session.updated");

            let second = ws
                .recv_message()
                .await
                .expect("second msg")
                .into_text()
                .expect("text");
            let second_json: Value = serde_json::from_str(&second).expect("json");
            assert_eq!(second_json["type"], "input_audio_buffer.append");

            let third = ws
                .recv_message()
                .await
                .expect("third msg")
                .into_text()
                .expect("text");
            let third_json: Value = serde_json::from_str(&third).expect("json");
            assert_eq!(third_json["type"], "conversation.item.create");
            assert_eq!(third_json["item"]["content"][0]["text"], "hello agent");

            let fourth = ws
                .recv_message()
                .await
                .expect("fourth msg")
                .into_text()
                .expect("text");
            let fourth_json: Value = serde_json::from_str(&fourth).expect("json");
            assert_eq!(fourth_json["type"], "conversation.handoff.append");
            assert_eq!(fourth_json["handoff_id"], "handoff_1");
            assert_eq!(fourth_json["output_text"], "hello from codex");

            ws.send_message(WsMessage::text(
                json!({
                    "type": "conversation.output_audio.delta",
                    "delta": "AQID",
                    "sample_rate": 48000,
                    "channels": 1
                })
                .to_string(),
            ))
            .await
            .expect("send audio");

            ws.send_message(WsMessage::text(
                json!({
                    "type": "conversation.input_transcript.delta",
                    "delta": "delegate "
                })
                .to_string(),
            ))
            .await
            .expect("send input transcript delta");

            ws.send_message(WsMessage::text(
                json!({
                    "type": "conversation.input_transcript.delta",
                    "delta": "now"
                })
                .to_string(),
            ))
            .await
            .expect("send input transcript delta");

            ws.send_message(WsMessage::text(
                json!({
                    "type": "conversation.output_transcript.delta",
                    "delta": "working"
                })
                .to_string(),
            ))
            .await
            .expect("send output transcript delta");

            ws.send_message(WsMessage::text(
                json!({
                    "type": "conversation.handoff.requested",
                    "handoff_id": "handoff_1",
                    "item_id": "item_2",
                    "input_transcript": "delegate now"
                })
                .to_string(),
            ))
            .await
            .expect("send item added");
        });

        let provider = Provider {
            name: "test".to_string(),
            base_url: format!("http://{addr}"),
            query_params: Some(HashMap::new()),
            headers: HeaderMap::new(),
            retry: crate::provider::RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: false,
                retry_transport: false,
            },
            stream_idle_timeout: Duration::from_secs(5),
        };
        let client = RealtimeWebsocketClient::new(provider);
        let connection = client
            .connect(
                RealtimeSessionConfig {
                    instructions: "backend prompt".to_string(),
                    model: Some("realtime-test-model".to_string()),
                    session_id: Some("conv_1".to_string()),
                    event_parser: RealtimeEventParser::V1,
                    session_mode: RealtimeSessionMode::Conversational,
                },
                HeaderMap::new(),
                HeaderMap::new(),
            )
            .await
            .expect("connect");

        let created = connection
            .next_event()
            .await
            .expect("next event")
            .expect("event");
        assert_eq!(
            created,
            RealtimeEvent::SessionUpdated {
                session_id: "sess_mock".to_string(),
                instructions: Some("backend prompt".to_string()),
            }
        );

        connection
            .send_audio_frame(RealtimeAudioFrame {
                data: "AQID".to_string(),
                sample_rate: 48000,
                num_channels: 1,
                samples_per_channel: Some(960),
            })
            .await
            .expect("send audio");
        connection
            .send_conversation_item_create("hello agent".to_string())
            .await
            .expect("send item");
        connection
            .send_conversation_handoff_append(
                "handoff_1".to_string(),
                "hello from codex".to_string(),
            )
            .await
            .expect("send handoff");

        let audio_event = connection
            .next_event()
            .await
            .expect("next event")
            .expect("event");
        assert_eq!(
            audio_event,
            RealtimeEvent::AudioOut(RealtimeAudioFrame {
                data: "AQID".to_string(),
                sample_rate: 48000,
                num_channels: 1,
                samples_per_channel: None,
            })
        );

        let input_delta_event = connection
            .next_event()
            .await
            .expect("next event")
            .expect("event");
        assert_eq!(
            input_delta_event,
            RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta {
                delta: "delegate ".to_string(),
            })
        );

        let input_delta_event = connection
            .next_event()
            .await
            .expect("next event")
            .expect("event");
        assert_eq!(
            input_delta_event,
            RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta {
                delta: "now".to_string(),
            })
        );

        let output_delta_event = connection
            .next_event()
            .await
            .expect("next event")
            .expect("event");
        assert_eq!(
            output_delta_event,
            RealtimeEvent::OutputTranscriptDelta(RealtimeTranscriptDelta {
                delta: "working".to_string(),
            })
        );

        let added_event = connection
            .next_event()
            .await
            .expect("next event")
            .expect("event");
        assert_eq!(
            added_event,
            RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
                handoff_id: "handoff_1".to_string(),
                item_id: "item_2".to_string(),
                input_transcript: "delegate now".to_string(),
                active_transcript: vec![
                    RealtimeTranscriptEntry {
                        role: "user".to_string(),
                        text: "delegate now".to_string(),
                    },
                    RealtimeTranscriptEntry {
                        role: "assistant".to_string(),
                        text: "working".to_string(),
                    },
                ],
            })
        );

        connection.close().await.expect("close");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn realtime_v2_session_update_includes_codex_tool_and_handoff_output_item() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_ws(stream).await;

            let first = ws
                .recv_message()
                .await
                .expect("first msg")
                .into_text()
                .expect("text");
            let first_json: Value = serde_json::from_str(&first).expect("json");
            assert_eq!(first_json["type"], "session.update");
            assert_eq!(
                first_json["session"]["type"],
                Value::String("realtime".to_string())
            );
            assert_eq!(
                first_json["session"]["audio"]["output"]["voice"],
                Value::String("alloy".to_string())
            );
            assert_eq!(
                first_json["session"]["tools"][0]["type"],
                Value::String("function".to_string())
            );
            assert_eq!(
                first_json["session"]["tools"][0]["name"],
                Value::String("codex".to_string())
            );
            assert_eq!(
                first_json["session"]["tools"][0]["parameters"]["required"],
                json!(["prompt"])
            );

            ws.send_message(WsMessage::text(
                json!({
                    "type": "session.updated",
                    "session": {"id": "sess_v2", "instructions": "backend prompt"}
                })
                .to_string(),
            ))
            .await
            .expect("send session.updated");

            let second = ws
                .recv_message()
                .await
                .expect("second msg")
                .into_text()
                .expect("text");
            let second_json: Value = serde_json::from_str(&second).expect("json");
            assert_eq!(second_json["type"], "conversation.item.create");
            assert_eq!(
                second_json["item"]["type"],
                Value::String("message".to_string())
            );
            assert_eq!(
                second_json["item"]["content"][0]["type"],
                Value::String("input_text".to_string())
            );
            assert_eq!(
                second_json["item"]["content"][0]["text"],
                Value::String("delegate this".to_string())
            );

            let third = ws
                .recv_message()
                .await
                .expect("third msg")
                .into_text()
                .expect("text");
            let third_json: Value = serde_json::from_str(&third).expect("json");
            assert_eq!(third_json["type"], "conversation.item.create");
            assert_eq!(
                third_json["item"]["type"],
                Value::String("function_call_output".to_string())
            );
            assert_eq!(
                third_json["item"]["call_id"],
                Value::String("call_1".to_string())
            );
            assert_eq!(
                third_json["item"]["output"],
                Value::String("delegated result".to_string())
            );
        });

        let provider = Provider {
            name: "test".to_string(),
            base_url: format!("http://{addr}"),
            query_params: Some(HashMap::new()),
            headers: HeaderMap::new(),
            retry: crate::provider::RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: false,
                retry_transport: false,
            },
            stream_idle_timeout: Duration::from_secs(5),
        };
        let client = RealtimeWebsocketClient::new(provider);
        let connection = client
            .connect(
                RealtimeSessionConfig {
                    instructions: "backend prompt".to_string(),
                    model: Some("realtime-test-model".to_string()),
                    session_id: Some("conv_1".to_string()),
                    event_parser: RealtimeEventParser::RealtimeV2,
                    session_mode: RealtimeSessionMode::Conversational,
                },
                HeaderMap::new(),
                HeaderMap::new(),
            )
            .await
            .expect("connect");

        let created = connection
            .next_event()
            .await
            .expect("next event")
            .expect("event");
        assert_eq!(
            created,
            RealtimeEvent::SessionUpdated {
                session_id: "sess_v2".to_string(),
                instructions: Some("backend prompt".to_string()),
            }
        );

        connection
            .send_conversation_item_create("delegate this".to_string())
            .await
            .expect("send text item");
        connection
            .send_conversation_handoff_append("call_1".to_string(), "delegated result".to_string())
            .await
            .expect("send handoff output");

        connection.close().await.expect("close");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn transcription_mode_session_update_omits_output_audio_and_instructions() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_ws(stream).await;

            let first = ws
                .recv_message()
                .await
                .expect("first msg")
                .into_text()
                .expect("text");
            let first_json: Value = serde_json::from_str(&first).expect("json");
            assert_eq!(first_json["type"], "session.update");
            assert_eq!(
                first_json["session"]["type"],
                Value::String("transcription".to_string())
            );
            assert!(first_json["session"].get("instructions").is_none());
            assert!(first_json["session"]["audio"].get("output").is_none());
            assert!(first_json["session"].get("tools").is_none());

            ws.send_message(WsMessage::text(
                json!({
                    "type": "session.updated",
                    "session": {"id": "sess_transcription"}
                })
                .to_string(),
            ))
            .await
            .expect("send session.updated");

            let second = ws
                .recv_message()
                .await
                .expect("second msg")
                .into_text()
                .expect("text");
            let second_json: Value = serde_json::from_str(&second).expect("json");
            assert_eq!(second_json["type"], "input_audio_buffer.append");
        });

        let provider = Provider {
            name: "test".to_string(),
            base_url: format!("http://{addr}"),
            query_params: Some(HashMap::new()),
            headers: HeaderMap::new(),
            retry: crate::provider::RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: false,
                retry_transport: false,
            },
            stream_idle_timeout: Duration::from_secs(5),
        };
        let client = RealtimeWebsocketClient::new(provider);
        let connection = client
            .connect(
                RealtimeSessionConfig {
                    instructions: "backend prompt".to_string(),
                    model: Some("realtime-test-model".to_string()),
                    session_id: Some("conv_1".to_string()),
                    event_parser: RealtimeEventParser::RealtimeV2,
                    session_mode: RealtimeSessionMode::Transcription,
                },
                HeaderMap::new(),
                HeaderMap::new(),
            )
            .await
            .expect("connect");

        let created = connection
            .next_event()
            .await
            .expect("next event")
            .expect("event");
        assert_eq!(
            created,
            RealtimeEvent::SessionUpdated {
                session_id: "sess_transcription".to_string(),
                instructions: None,
            }
        );

        connection
            .send_audio_frame(RealtimeAudioFrame {
                data: "AQID".to_string(),
                sample_rate: 24_000,
                num_channels: 1,
                samples_per_channel: Some(480),
            })
            .await
            .expect("send audio");

        connection.close().await.expect("close");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn v1_transcription_mode_is_treated_as_conversational() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_ws(stream).await;

            let first = ws
                .recv_message()
                .await
                .expect("first msg")
                .into_text()
                .expect("text");
            let first_json: Value = serde_json::from_str(&first).expect("json");
            assert_eq!(first_json["type"], "session.update");
            assert_eq!(
                first_json["session"]["type"],
                Value::String("quicksilver".to_string())
            );
            assert_eq!(
                first_json["session"]["instructions"],
                Value::String("backend prompt".to_string())
            );
            assert_eq!(
                first_json["session"]["audio"]["output"]["voice"],
                Value::String("fathom".to_string())
            );
            assert!(first_json["session"].get("tools").is_none());

            ws.send_message(WsMessage::text(
                json!({
                    "type": "session.updated",
                    "session": {"id": "sess_v1_mode"}
                })
                .to_string(),
            ))
            .await
            .expect("send session.updated");
        });

        let provider = Provider {
            name: "test".to_string(),
            base_url: format!("http://{addr}"),
            query_params: Some(HashMap::new()),
            headers: HeaderMap::new(),
            retry: crate::provider::RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: false,
                retry_transport: false,
            },
            stream_idle_timeout: Duration::from_secs(5),
        };
        let client = RealtimeWebsocketClient::new(provider);
        let connection = client
            .connect(
                RealtimeSessionConfig {
                    instructions: "backend prompt".to_string(),
                    model: Some("realtime-test-model".to_string()),
                    session_id: Some("conv_1".to_string()),
                    event_parser: RealtimeEventParser::V1,
                    session_mode: RealtimeSessionMode::Transcription,
                },
                HeaderMap::new(),
                HeaderMap::new(),
            )
            .await
            .expect("connect");

        let created = connection
            .next_event()
            .await
            .expect("next event")
            .expect("event");
        assert_eq!(
            created,
            RealtimeEvent::SessionUpdated {
                session_id: "sess_v1_mode".to_string(),
                instructions: None,
            }
        );

        connection.close().await.expect("close");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn send_does_not_block_while_next_event_waits_for_inbound_data() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_ws(stream).await;

            let first = ws
                .recv_message()
                .await
                .expect("first msg")
                .into_text()
                .expect("text");
            let first_json: Value = serde_json::from_str(&first).expect("json");
            assert_eq!(first_json["type"], "session.update");

            let second = ws
                .recv_message()
                .await
                .expect("second msg")
                .into_text()
                .expect("text");
            let second_json: Value = serde_json::from_str(&second).expect("json");
            assert_eq!(second_json["type"], "input_audio_buffer.append");

            ws.send_message(WsMessage::text(
                json!({
                    "type": "session.updated",
                    "session": {"id": "sess_after_send", "instructions": "backend prompt"}
                })
                .to_string(),
            ))
            .await
            .expect("send session.updated");
        });

        let provider = Provider {
            name: "test".to_string(),
            base_url: format!("http://{addr}"),
            query_params: Some(HashMap::new()),
            headers: HeaderMap::new(),
            retry: crate::provider::RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: false,
                retry_transport: false,
            },
            stream_idle_timeout: Duration::from_secs(5),
        };
        let client = RealtimeWebsocketClient::new(provider);
        let connection = client
            .connect(
                RealtimeSessionConfig {
                    instructions: "backend prompt".to_string(),
                    model: Some("realtime-test-model".to_string()),
                    session_id: Some("conv_1".to_string()),
                    event_parser: RealtimeEventParser::V1,
                    session_mode: RealtimeSessionMode::Conversational,
                },
                HeaderMap::new(),
                HeaderMap::new(),
            )
            .await
            .expect("connect");

        let (send_result, next_result) = tokio::join!(
            async {
                tokio::time::timeout(
                    Duration::from_millis(200),
                    connection.send_audio_frame(RealtimeAudioFrame {
                        data: "AQID".to_string(),
                        sample_rate: 48000,
                        num_channels: 1,
                        samples_per_channel: Some(960),
                    }),
                )
                .await
            },
            connection.next_event()
        );

        send_result
            .expect("send should not block on next_event")
            .expect("send audio");
        let next_event = next_result.expect("next event").expect("event");
        assert_eq!(
            next_event,
            RealtimeEvent::SessionUpdated {
                session_id: "sess_after_send".to_string(),
                instructions: Some("backend prompt".to_string()),
            }
        );

        connection.close().await.expect("close");
        server.await.expect("server task");
    }
}
