use super::*;
use agent_client_protocol::schema::v1::{
    CancelNotification, NewSessionRequest, PromptRequest, SessionId,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

struct Peer {
    reader: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
}

impl Peer {
    fn new(stream: DuplexStream) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(reader).lines(),
            writer,
        }
    }

    async fn receive(&mut self) -> Option<Value> {
        self.reader
            .next_line()
            .await
            .expect("read peer line")
            .map(|line| serde_json::from_str(&line).expect("valid peer JSON"))
    }

    async fn send(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).expect("serialize peer message");
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.expect("write peer");
        self.writer.flush().await.expect("flush peer");
    }
}

async fn connect(
    options: ConnectionOptions,
) -> (AcpConnection, mpsc::Receiver<SessionNotification>, Peer) {
    let (client, server) = tokio::io::duplex(16 * 1024);
    let (reader, writer) = tokio::io::split(client);
    let mut peer = Peer::new(server);
    let (connected, ()) = tokio::join!(AcpConnection::connect(reader, writer, options), async {
        let initialize = peer.receive().await.expect("initialize request");
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(initialize["params"]["protocolVersion"], 1);
        assert_ne!(initialize["params"]["clientCapabilities"]["terminal"], true);
        assert_ne!(
            initialize["params"]["clientCapabilities"]["fs"]["readTextFile"],
            true
        );
        peer.send(json!({
            "jsonrpc": "2.0", "id": initialize["id"],
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": { "loadSession": true },
                "authMethods": [],
                "agentInfo": { "name": "fake-agent", "version": "1" }
            }
        }))
        .await;
    });
    let (connection, updates) = connected.expect("connect fake agent");
    (connection, updates, peer)
}

fn update(text: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text }
            }
        }
    })
}

async fn wait_closed(client: &AcpClient) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !client.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("connection closes");
}

#[tokio::test]
async fn negotiates_and_correlates_requests_without_authenticating() {
    let (connection, _updates, mut peer) = connect(ConnectionOptions::default()).await;
    assert!(connection.initialized().agent_capabilities.load_session);
    let client = connection.client();
    let (response, ()) = tokio::join!(
        client.request(NewSessionRequest::new(PathBuf::from("/workspace"))),
        async {
            let request = peer.receive().await.expect("session request");
            assert_eq!(request["method"], "session/new");
            assert_eq!(request["params"]["cwd"], "/workspace");
            peer.send(json!({
                "jsonrpc": "2.0", "id": request["id"],
                "result": { "sessionId": "session-1" }
            }))
            .await;
        }
    );
    assert_eq!(
        response.expect("session response").session_id,
        SessionId::new("session-1")
    );
    connection.disconnect().await.expect("disconnect");
    assert!(client.is_closed());
}

#[tokio::test]
async fn streams_updates_and_denies_permission_while_prompt_is_pending() {
    let (connection, mut updates, mut peer) = connect(ConnectionOptions::default()).await;
    let client = connection.client();
    let (response, ()) = tokio::join!(
        client.request(PromptRequest::new(SessionId::new("session-1"), vec![])),
        async {
            let prompt = peer.receive().await.expect("prompt request");
            assert_eq!(prompt["method"], "session/prompt");
            peer.send(update("hello")).await;
            assert_eq!(
                serde_json::to_value(updates.recv().await.expect("streamed update"))
                    .expect("serialize update")["update"]["content"]["text"],
                "hello"
            );
            peer.send(json!({
                "jsonrpc": "2.0", "id": "permission-1",
                "method": "session/request_permission",
                "params": {
                    "sessionId": "session-1",
                    "toolCall": { "toolCallId": "tool-1", "title": "Run native command" },
                    "options": [
                        { "optionId": "yes", "name": "Allow", "kind": "allow_once" }
                    ]
                }
            }))
            .await;
            let denied = peer.receive().await.expect("permission response");
            assert_eq!(denied["id"], "permission-1");
            assert_eq!(denied["result"]["outcome"]["outcome"], "cancelled");
            peer.send(json!({
                "jsonrpc": "2.0", "id": prompt["id"],
                "result": { "stopReason": "end_turn" }
            }))
            .await;
        }
    );
    assert!(response.is_ok(), "{response:?}");
    connection.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn request_timeout_closes_connection_and_never_replays() {
    let options = ConnectionOptions {
        request_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let (connection, _updates, mut peer) = connect(options).await;
    let client = connection.client();
    let (result, submitted) = tokio::join!(
        client.request(NewSessionRequest::new(PathBuf::from("/workspace"))),
        peer.receive(),
    );
    assert_eq!(submitted.expect("one submission")["method"], "session/new");
    assert!(matches!(result, Err(GuestError::Timeout)));
    assert!(matches!(
        client
            .request(NewSessionRequest::new(PathBuf::from("/workspace")))
            .await,
        Err(GuestError::Disconnected)
    ));
    drop(connection);
    assert!(peer.receive().await.is_none(), "no second submission");
}

#[tokio::test]
async fn dropping_an_inflight_request_closes_the_connection() {
    let (connection, _updates, mut peer) = connect(ConnectionOptions::default()).await;
    let client = connection.client();
    {
        let request = client.request(NewSessionRequest::new(PathBuf::from("/workspace")));
        tokio::pin!(request);
        tokio::select! {
            result = &mut request => panic!("peer did not respond: {result:?}"),
            submitted = peer.receive() => {
                assert_eq!(submitted.expect("submission")["method"], "session/new");
            }
        }
    }
    assert!(client.is_closed(), "dropped future is an uncertain request");
    drop(connection);
}

#[tokio::test]
async fn unread_update_overflow_fails_closed() {
    let options = ConnectionOptions {
        notification_capacity: 1,
        ..Default::default()
    };
    let (connection, _updates, mut peer) = connect(options).await;
    let client = connection.client();
    peer.send(update("first")).await;
    peer.send(update("second")).await;
    wait_closed(&client).await;
    drop(connection);
}

#[tokio::test]
async fn oversized_incoming_lines_close_the_connection() {
    let options = ConnectionOptions {
        max_message_bytes: 1024,
        ..Default::default()
    };
    let (connection, _updates, mut peer) = connect(options).await;
    let client = connection.client();
    peer.writer
        .write_all(&vec![b'x'; 1026])
        .await
        .expect("oversized frame");
    peer.writer.flush().await.expect("flush frame");
    wait_closed(&client).await;
    drop(connection);
}

#[tokio::test]
async fn peer_eof_invalidates_cloned_handles() {
    let (connection, _updates, peer) = connect(ConnectionOptions::default()).await;
    let client = connection.client();
    drop(peer);
    wait_closed(&client).await;
    assert!(matches!(
        client
            .request(NewSessionRequest::new(PathBuf::from("/workspace")))
            .await,
        Err(GuestError::Disconnected)
    ));
    drop(connection);
}

#[tokio::test]
async fn zero_limits_are_rejected_without_starting_a_driver() {
    let (stream, _peer) = tokio::io::duplex(1024);
    let (reader, writer) = tokio::io::split(stream);
    let result = AcpConnection::connect(
        reader,
        writer,
        ConnectionOptions {
            notification_capacity: 0,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(result, Err(GuestError::InvalidOptions)));
}

#[tokio::test]
async fn dropping_connect_during_initialization_closes_the_transport() {
    let (stream, server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(stream);
    let mut peer = Peer::new(server);
    {
        let connecting = AcpConnection::connect(reader, writer, ConnectionOptions::default());
        tokio::pin!(connecting);
        tokio::select! {
            _ = &mut connecting => panic!("peer did not initialize"),
            submitted = peer.receive() => {
                assert_eq!(submitted.expect("initialize request")["method"], "initialize");
            }
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(2), peer.receive())
            .await
            .expect("cancelled constructor closes pipes")
            .is_none()
    );
}

#[tokio::test]
async fn cancellation_is_delivered_while_a_prompt_is_pending() {
    let (connection, _updates, mut peer) = connect(ConnectionOptions::default()).await;
    let client = connection.client();
    let (response, ()) = tokio::join!(
        client.request(PromptRequest::new(SessionId::new("session-1"), vec![])),
        async {
            let prompt = peer.receive().await.expect("prompt request");
            client
                .notify(CancelNotification::new(SessionId::new("session-1")))
                .expect("queue cancellation");
            let cancel = peer.receive().await.expect("cancel notification");
            assert_eq!(cancel["method"], "session/cancel");
            assert_eq!(cancel["params"]["sessionId"], "session-1");
            assert!(cancel.get("id").is_none(), "cancellation is not an RPC ack");
            peer.send(json!({
                "jsonrpc": "2.0", "id": prompt["id"],
                "result": { "stopReason": "cancelled" }
            }))
            .await;
        }
    );
    let response =
        serde_json::to_value(response.expect("cancelled prompt")).expect("serialize response");
    assert_eq!(response["stopReason"], "cancelled");
    connection.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn malformed_json_closes_the_transport() {
    let (connection, _updates, mut peer) = connect(ConnectionOptions::default()).await;
    let client = connection.client();
    peer.writer
        .write_all(b"this is not json\n")
        .await
        .expect("write malformed frame");
    wait_closed(&client).await;
    drop(connection);
}

#[tokio::test]
async fn oversized_outgoing_request_is_not_sent_or_retried() {
    use agent_client_protocol::schema::v1::{ContentBlock, TextContent};
    let options = ConnectionOptions {
        max_message_bytes: 1024,
        ..Default::default()
    };
    let (connection, _updates, mut peer) = connect(options).await;
    let client = connection.client();
    let result = client
        .request(PromptRequest::new(
            SessionId::new("session-1"),
            vec![ContentBlock::Text(TextContent::new("x".repeat(2048)))],
        ))
        .await;
    assert!(result.is_err());
    wait_closed(&client).await;
    drop(connection);
    assert!(
        peer.receive().await.is_none(),
        "oversized prompt never sent"
    );
}

#[tokio::test]
async fn incompatible_version_fails_and_closes_the_transport() {
    let (stream, server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(stream);
    let mut peer = Peer::new(server);
    let (result, ()) = tokio::join!(
        AcpConnection::connect(reader, writer, ConnectionOptions::default()),
        async {
            let initialize = peer.receive().await.expect("initialize");
            peer.send(json!({
                "jsonrpc": "2.0", "id": initialize["id"],
                "result": {
                    "protocolVersion": 2,
                    "agentCapabilities": {},
                    "authMethods": []
                }
            }))
            .await;
        }
    );
    assert!(matches!(result, Err(GuestError::VersionMismatch)));
    assert!(peer.receive().await.is_none());
}

#[tokio::test]
async fn an_explicit_application_permission_handler_is_called() {
    use std::sync::atomic::AtomicUsize;

    #[derive(Debug, Default)]
    struct CountAndDeny(AtomicUsize);

    impl PermissionHandler for CountAndDeny {
        fn request_permission(
            &self,
            _request: RequestPermissionRequest,
        ) -> protocol::BoxFuture<'_, protocol::Result<RequestPermissionResponse>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ))
            })
        }
    }

    let policy = Arc::new(CountAndDeny::default());
    let (connection, _updates, mut peer) = connect(ConnectionOptions {
        permissions: policy.clone(),
        ..Default::default()
    })
    .await;
    peer.send(json!({
        "jsonrpc": "2.0", "id": "permission-application",
        "method": "session/request_permission",
        "params": {
            "sessionId": "session-1",
            "toolCall": { "toolCallId": "tool-1" },
            "options": []
        }
    }))
    .await;
    let response = peer.receive().await.expect("application decision");
    assert_eq!(response["result"]["outcome"]["outcome"], "cancelled");
    assert_eq!(policy.0.load(Ordering::Relaxed), 1);
    connection.disconnect().await.expect("disconnect");
}
