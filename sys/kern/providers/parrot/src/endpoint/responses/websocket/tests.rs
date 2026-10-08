//! Behavioral tests against a real loopback HTTP/WebSocket peer.
//! Assertions concern requests, responses, connection reuse and isolation.
use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use futures::{SinkExt, StreamExt};
use jiff::SignedDuration;
use rama::ServiceInput;
use rama::http::ws::protocol::{Message, Role};
use rama::http::ws::runtime::AsyncWebSocket;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::task::AbortOnDropHandle;

use super::ResponsesWebSocket;
use crate::common::{ResponseEvent, ResponsesApiRequest};
use crate::openai::StaticAuthProvider;
use crate::{Provider, RamaTransport, ResponsesClient, ResponsesOptions};

const TEST_DEADLINE: SignedDuration = SignedDuration::from_secs(5);
// Longer than TEST_DEADLINE: an idle timeout must not disguise a broken
// cancellation path as successful cleanup.
const PEER_IDLE_TIMEOUT: SignedDuration = SignedDuration::from_secs(60);
const PREWARM_OBSERVATION_WINDOW: SignedDuration = SignedDuration::from_millis(100);
const MAX_HTTP_HEADER_BYTES: usize = 32 * 1024;

struct Peer {
    url: String,
    upgrades: mpsc::UnboundedReceiver<(usize, HashMap<String, String>)>,
    requests: mpsc::UnboundedReceiver<(usize, Value)>,
    posts: mpsc::UnboundedReceiver<Value>,
    _task: AbortOnDropHandle<()>,
}

impl Peer {
    async fn start(reject_upgrade: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (upgrade_tx, upgrades) = mpsc::unbounded_channel();
        let (request_tx, requests) = mpsc::unbounded_channel();
        let (post_tx, posts) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            let mut id = 0;
            loop {
                tokio::select! {
                    connection = listener.accept() => {
                        let (socket, _) = connection.unwrap();
                        id += 1;
                        let upgrade_tx = upgrade_tx.clone();
                        let request_tx = request_tx.clone();
                        let post_tx = post_tx.clone();
                        connections.spawn(serve(
                            socket, id, reject_upgrade, upgrade_tx, request_tx, post_tx,
                        ));
                    }
                    Some(result) = connections.join_next() => { result.unwrap(); }
                }
            }
        });
        Self {
            url,
            upgrades,
            requests,
            posts,
            _task: AbortOnDropHandle::new(task),
        }
    }

    fn client(
        &self,
        transport: Arc<ResponsesWebSocket>,
        token: &str,
    ) -> ResponsesClient<RamaTransport, StaticAuthProvider> {
        let mut provider = Provider::from_base_url_with_default_streaming_config(
            "OpenAI",
            self.url.clone(),
            false,
        );
        provider.retry.max_attempts = 1;
        provider.stream_idle_timeout = PEER_IDLE_TIMEOUT.unsigned_abs();
        ResponsesClient::new(
            RamaTransport::default_client(),
            provider,
            StaticAuthProvider::new(Some(token.into()), Some("test-account".into())),
        )
        .with_websocket(transport)
    }

    async fn request(&mut self) -> (usize, Value) {
        tokio::time::timeout(TEST_DEADLINE.unsigned_abs(), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn upgrade(&mut self) -> (usize, HashMap<String, String>) {
        tokio::time::timeout(TEST_DEADLINE.unsigned_abs(), self.upgrades.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

async fn serve(
    mut stream: TcpStream,
    id: usize,
    reject_upgrade: bool,
    upgrade_tx: mpsc::UnboundedSender<(usize, HashMap<String, String>)>,
    request_tx: mpsc::UnboundedSender<(usize, Value)>,
    post_tx: mpsc::UnboundedSender<Value>,
) {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        if stream.read_exact(&mut byte).await.is_err() {
            return;
        }
        head.push(byte[0]);
        assert!(head.len() < MAX_HTTP_HEADER_BYTES);
    }
    let head = String::from_utf8(head).unwrap();
    let headers: HashMap<_, _> = head
        .lines()
        .skip(1)
        .filter_map(|line| {
            line.split_once(':')
                .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    if head.starts_with("POST ") {
        let length: usize = headers["content-length"].parse().unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.unwrap();
        post_tx
            .send(serde_json::from_slice(&body).unwrap())
            .unwrap();
        let body = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({"type": "response.output_text.delta", "delta": "http"}),
            json!({"type": "response.completed", "response": {"id": "http-response"}})
        );
        stream.write_all(format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len(),
        ).as_bytes()).await.unwrap();
        return;
    }
    upgrade_tx.send((id, headers.clone())).unwrap();
    if reject_upgrade {
        stream
            .write_all(
                b"HTTP/1.1 426 Upgrade Required\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        return;
    }
    let accept = base64::engine::general_purpose::STANDARD.encode(Sha1::digest(
        format!(
            "{}258EAFA5-E914-47DA-95CA-C5AB0DC85B11",
            headers["sec-websocket-key"],
        )
        .as_bytes(),
    ));
    stream.write_all(format!(
        "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {accept}\r\nopenai-model: server-model\r\nx-codex-turn-state: test-turn-state\r\n\r\n",
    ).as_bytes()).await.unwrap();
    let mut socket =
        AsyncWebSocket::from_raw_socket(ServiceInput::new(stream), Role::Server, None).await;
    while let Some(Ok(message)) = socket.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let request: Value = serde_json::from_str(text.as_str()).unwrap();
        assert_eq!(request["type"], "response.create");
        assert!(
            request.get("stream").is_none(),
            "v2 does not accept the HTTP stream parameter"
        );
        request_tx.send((id, request.clone())).unwrap();
        if socket
            .send(Message::text(
                json!({
                    "type": "response.output_text.delta", "delta": "hello",
                })
                .to_string(),
            ))
            .await
            .is_err()
        {
            return;
        }
        if request["model"] == "drop" {
            return;
        }
        if request["model"] == "stall" {
            while socket.next().await.is_some() {}
            return;
        }
        if socket
            .send(Message::text(
                json!({
                    "type": "response.completed", "response": {
                        "id": format!("response-{id}"),
                        "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5},
                    },
                })
                .to_string(),
            ))
            .await
            .is_err()
        {
            return;
        }
    }
}

fn request(model: &str) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: model.into(),
        instructions: "instructions".into(),
        input: vec![
            serde_json::from_value(json!({
                "type": "message", "role": "user",
                "content": [{"type": "input_text", "text": "hello"}],
            }))
            .unwrap(),
        ],
        tools: vec![],
        tool_choice: "auto".into(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream: true,
        include: vec![],
        service_tier: None,
        prompt_cache_key: None,
        text: None,
    }
}

async fn completed(
    client: &ResponsesClient<RamaTransport, StaticAuthProvider>,
    request: ResponsesApiRequest,
    options: ResponsesOptions,
) -> Vec<ResponseEvent> {
    tokio::time::timeout(TEST_DEADLINE.unsigned_abs(), async {
        let mut stream = client.stream_request(request, options).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ResponseEvent::Completed { .. }))
        );
        events
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn prewarm_sends_no_generation_and_tool_rounds_reuse_one_socket_with_current_full_input() {
    let mut peer = Peer::start(false).await;
    let client = peer.client(Arc::new(ResponsesWebSocket::default()), "token-one");
    let options = ResponsesOptions::default();
    client.prewarm(&options);
    let (connection, headers) = peer.upgrade().await;
    assert_eq!(headers["authorization"], "Bearer token-one");
    assert_eq!(headers["chatgpt-account-id"], "test-account");
    assert!(headers["openai-beta"].contains("responses_websockets=2026-02-06"));
    assert!(
        tokio::time::timeout(
            PREWARM_OBSERVATION_WINDOW.unsigned_abs(),
            peer.requests.recv()
        )
        .await
        .is_err()
    );

    completed(&client, request("first-model"), options.clone()).await;
    let (first_connection, first) = peer.request().await;
    assert_eq!(first_connection, connection);
    assert_eq!(
        first["input"],
        serde_json::to_value(request("first-model").input).unwrap()
    );

    let mut changed = request("different-model");
    changed.input.clear(); // History rollback must not use previous_response_id.
    changed.tools = vec![
        json!({"type": "function", "name": "newly_activated", "parameters": {"type": "object"}}),
    ];
    completed(&client, changed, options).await;
    let (second_connection, second) = peer.request().await;
    assert_eq!(second_connection, first_connection);
    assert_eq!(second["model"], "different-model");
    assert_eq!(second["input"], json!([]));
    assert_eq!(second["tools"][0]["name"], "newly_activated");
    assert!(second.get("previous_response_id").is_none());
    assert!(peer.upgrades.try_recv().is_err());
    assert!(peer.posts.try_recv().is_err());
}

#[tokio::test]
async fn search_activation_reconnects_only_when_required_and_preserves_full_history() {
    for reconnect in [false, true] {
        let mut peer = Peer::start(false).await;
        let client = peer.client(Arc::new(ResponsesWebSocket::default()), "fixture-token");
        let options = ResponsesOptions {
            websocket_reconnect_on_web_search_activation: reconnect,
            ..Default::default()
        };
        client.prewarm(&options);
        let (prewarmed, _) = peer.upgrade().await;
        completed(&client, request("first"), options.clone()).await;
        let (original, _) = peer.request().await;
        assert_eq!(original, prewarmed);

        // An ordinary function named web_search is not a hosted tool.
        let mut function = request("function");
        function.tools = vec![json!({
            "type": "function", "name": "web_search", "parameters": {"type": "object"}
        })];
        completed(&client, function, options.clone()).await;
        assert_eq!(peer.request().await.0, original);

        let mut search = request("search");
        search.tools = vec![json!({"type": "web_search", "external_web_access": true})];
        let expected_input = serde_json::to_value(&search.input).unwrap();
        completed(&client, search.clone(), options.clone()).await;
        let (with_search, wire) = peer.request().await;
        assert_eq!(with_search != original, reconnect);
        assert_eq!(wire["input"], expected_input);
        assert_eq!(wire["tools"], json!(search.tools));
        assert!(wire.get("previous_response_id").is_none());
        if reconnect {
            assert_eq!(peer.upgrade().await.0, with_search);
        }

        // Once initialized with search, removing and restoring it is safe.
        completed(&client, request("without-search"), options.clone()).await;
        assert_eq!(peer.request().await.0, with_search);
        search.tools[0]["external_web_access"] = json!(false);
        completed(&client, search, options).await;
        assert_eq!(peer.request().await.0, with_search);
        assert!(peer.upgrades.try_recv().is_err());
        assert!(peer.requests.try_recv().is_err(), "no request replay");
        assert!(peer.posts.try_recv().is_err(), "no HTTP fallback");
    }
}

#[tokio::test]
async fn sessions_and_changed_credentials_or_routing_headers_get_separate_connections() {
    let mut peer = Peer::start(false).await;
    let session = Arc::new(ResponsesWebSocket::default());
    let first = peer.client(session.clone(), "first-token");
    let other_session = peer.client(Arc::new(ResponsesWebSocket::default()), "first-token");
    completed(&first, request("test"), ResponsesOptions::default()).await;
    let (original, _) = peer.request().await;
    completed(&other_session, request("test"), ResponsesOptions::default()).await;
    assert_ne!(peer.request().await.0, original);

    let rotated = peer.client(session.clone(), "rotated-token");
    completed(&rotated, request("test"), ResponsesOptions::default()).await;
    let (rotated_connection, _) = peer.request().await;
    assert_ne!(rotated_connection, original);
    let mut options = ResponsesOptions::default();
    options.extra_headers.insert(
        "x-codex-turn-metadata",
        "new-routing-metadata".parse().unwrap(),
    );
    completed(&rotated, request("test"), options.clone()).await;
    let routed_connection = peer.request().await.0;
    assert_ne!(routed_connection, rotated_connection);
    session.reset().await;
    completed(&rotated, request("test"), options).await;
    assert_ne!(peer.request().await.0, routed_connection);
}

#[tokio::test]
async fn rejected_v2_upgrade_is_an_error_and_never_downgrades_to_http() {
    let mut peer = Peer::start(true).await;
    let client = peer.client(Arc::new(ResponsesWebSocket::default()), "token");
    for _ in 0..2 {
        let result = client
            .stream_request(request("test"), ResponsesOptions::default())
            .await;
        assert!(matches!(result, Err(crate::ApiError::Api { status, .. })
            if status == rama::http::StatusCode::UPGRADE_REQUIRED));
        peer.upgrade().await;
    }
    assert!(peer.upgrades.try_recv().is_err());
    assert!(peer.requests.try_recv().is_err());
    assert!(peer.posts.try_recv().is_err());
}

#[tokio::test]
async fn errors_after_acceptance_do_not_resend_over_http_and_cancellation_discards_socket() {
    let mut peer = Peer::start(false).await;
    let client = peer.client(Arc::new(ResponsesWebSocket::default()), "token");
    let mut stream = client
        .stream_request(request("drop"), ResponsesOptions::default())
        .await
        .unwrap();
    let (broken, _) = peer.request().await;
    let mut saw_delta = false;
    let mut saw_error = false;
    while let Some(event) = tokio::time::timeout(TEST_DEADLINE.unsigned_abs(), stream.next())
        .await
        .unwrap()
    {
        match event {
            Ok(ResponseEvent::OutputTextDelta(_)) => saw_delta = true,
            Err(_) => saw_error = true,
            _ => {}
        }
    }
    assert!(saw_delta && saw_error);
    assert!(
        peer.posts.try_recv().is_err(),
        "no duplicate HTTP generation"
    );

    let mut stalled = client
        .stream_request(request("stall"), ResponsesOptions::default())
        .await
        .unwrap();
    let (stalled_connection, _) = peer.request().await;
    assert_ne!(stalled_connection, broken);
    while !matches!(
        stalled.next().await.unwrap().unwrap(),
        ResponseEvent::OutputTextDelta(_)
    ) {}
    drop(stalled);
    completed(&client, request("healthy"), ResponsesOptions::default()).await;
    assert_ne!(peer.request().await.0, stalled_connection);
    assert!(peer.posts.try_recv().is_err());
}

#[tokio::test]
async fn dropping_abi_adapter_stream_releases_silent_session_socket() {
    use crate::openai::OpenAiAdapter;
    use chaos_abi::{ModelAdapter, TurnEvent, TurnRequest};

    let mut peer = Peer::start(false).await;
    let session = Arc::new(ResponsesWebSocket::default());
    let client = peer.client(session.clone(), "token");
    let mut provider =
        Provider::from_base_url_with_default_streaming_config("OpenAI", peer.url.clone(), false);
    provider.stream_idle_timeout = PEER_IDLE_TIMEOUT.unsigned_abs();
    let adapter = OpenAiAdapter::new(
        RamaTransport::default_client(),
        provider,
        StaticAuthProvider::new(Some("token".into()), Some("test-account".into())),
        None,
        crate::SessionRepresenter::openai(),
    )
    .with_websocket(session);
    let mut stream = adapter
        .stream(TurnRequest {
            model: "stall".into(),
            instructions: "instructions".into(),
            input: vec![],
            tools: vec![],
            parallel_tool_calls: true,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions: Default::default(),
        })
        .await
        .unwrap();
    let (stalled, _) = peer.request().await;
    tokio::time::timeout(TEST_DEADLINE.unsigned_abs(), async {
        while !matches!(
            stream.next().await.unwrap().unwrap(),
            TurnEvent::OutputTextDelta(_)
        ) {}
    })
    .await
    .unwrap();
    drop(stream);
    completed(&client, request("healthy"), ResponsesOptions::default()).await;
    assert_ne!(peer.request().await.0, stalled);
    assert!(peer.posts.try_recv().is_err());
}
