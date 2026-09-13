use std::collections::VecDeque;

use tokio::sync::Mutex;

use super::*;
use crate::handler::NoopClientHandler;
use crate::protocol::Implementation;
use crate::protocol::ServerCapabilities;
use crate::transport::TransportFuture;

struct MockTransport {
    sent: Mutex<Vec<JsonRpcMessage>>,
    incoming: Mutex<VecDeque<JsonRpcMessage>>,
}

impl MockTransport {
    fn new(incoming: Vec<JsonRpcMessage>) -> Arc<Self> {
        Arc::new(Self {
            sent: Mutex::new(Vec::new()),
            incoming: Mutex::new(VecDeque::from(incoming)),
        })
    }
}

impl MessageTransport for MockTransport {
    fn send<'a>(&'a self, message: JsonRpcMessage) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            self.sent.lock().await.push(message);
            Ok(())
        })
    }

    fn recv<'a>(&'a self) -> TransportFuture<'a, JsonRpcMessage> {
        Box::pin(async move {
            self.incoming
                .lock()
                .await
                .pop_front()
                .ok_or(GuestError::Disconnected)
        })
    }

    fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn handshake_accepts_previous_version_and_replies_to_ping() {
    let transport = MockTransport::new(vec![
        JsonRpcMessage::Request(JsonRpcRequest::new(
            serde_json::json!("ping-1"),
            "ping",
            Some(serde_json::json!({})),
        )),
        JsonRpcMessage::Response(JsonRpcResponse::success(
            serde_json::json!(1),
            serde_json::to_value(InitializeResult {
                protocol_version: "2025-06-18".to_string(),
                capabilities: ServerCapabilities::default(),
                server_info: Implementation::new("example-server", "1.0.0"),
                instructions: None,
            })
            .unwrap(),
        )),
    ]);

    let info = perform_handshake(
        Arc::clone(&transport) as Arc<dyn MessageTransport>,
        &ConnectionOptions {
            client_info: Implementation::new("test-client", "1.0.0"),
            capabilities: ClientCapabilities::default(),
            handler: Arc::new(NoopClientHandler),
            default_timeout: Duration::from_secs(30),
        },
    )
    .await
    .unwrap();

    assert_eq!(info.protocol_version, "2025-06-18");

    let sent = transport.sent.lock().await;
    assert_eq!(sent.len(), 3);
    assert!(matches!(&sent[0], JsonRpcMessage::Request(request) if request.method == "initialize"));
    assert!(
        matches!(&sent[1], JsonRpcMessage::Response(response) if response.id == Some(serde_json::json!("ping-1")))
    );
    assert!(
        matches!(&sent[2], JsonRpcMessage::Notification(notification) if notification.method == "notifications/initialized")
    );
}

struct PendingTransport;

impl MessageTransport for PendingTransport {
    fn send<'a>(&'a self, _message: JsonRpcMessage) -> TransportFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn recv<'a>(&'a self) -> TransportFuture<'a, JsonRpcMessage> {
        Box::pin(std::future::pending())
    }

    fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn connect_times_out_when_server_never_answers_initialize() {
    let result = connect_with_transport(
        Arc::new(PendingTransport),
        ConnectionOptions {
            client_info: Implementation::new("test-client", "1.0.0"),
            capabilities: ClientCapabilities::default(),
            handler: Arc::new(NoopClientHandler),
            default_timeout: Duration::from_millis(50),
        },
    )
    .await;

    assert!(matches!(result, Err(GuestError::Timeout(_))));
}

struct SlowSendTransport {
    incoming_tx: mpsc::UnboundedSender<JsonRpcMessage>,
    incoming_rx: Mutex<mpsc::UnboundedReceiver<JsonRpcMessage>>,
}

impl SlowSendTransport {
    fn new() -> Arc<Self> {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            incoming_tx,
            incoming_rx: Mutex::new(incoming_rx),
        })
    }
}

impl MessageTransport for SlowSendTransport {
    fn send<'a>(&'a self, message: JsonRpcMessage) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            if let JsonRpcMessage::Request(request) = &message {
                if request.method == "slow" {
                    std::future::pending::<()>().await;
                }
                let _ = self
                    .incoming_tx
                    .send(JsonRpcMessage::Response(JsonRpcResponse::success(
                        request.id.clone().unwrap(),
                        serde_json::json!({"ok": true}),
                    )));
            }
            Ok(())
        })
    }

    fn recv<'a>(&'a self) -> TransportFuture<'a, JsonRpcMessage> {
        Box::pin(async move {
            match self.incoming_rx.lock().await.recv().await {
                Some(message) => Ok(message),
                None => std::future::pending().await,
            }
        })
    }

    fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn slow_request_send_does_not_block_other_requests() {
    let transport = SlowSendTransport::new();
    let shared = Arc::new(SharedState::new(
        ServerInfo {
            server_info: Implementation::new("test-server", "1.0.0"),
            protocol_version: "2025-11-25".to_string(),
            capabilities: ServerCapabilities::default(),
            instructions: None,
        },
        Duration::from_secs(5),
    ));
    let (command_tx, command_rx) = mpsc::channel(8);
    let runtime = tokio::spawn(run_runtime(
        Arc::clone(&transport) as Arc<dyn MessageTransport>,
        shared,
        Arc::new(NoopClientHandler),
        command_rx,
    ));

    let (slow_tx, mut slow_rx) = oneshot::channel();
    command_tx
        .send(RuntimeCommand::Request {
            request_id: RequestId::number(10),
            method: "slow".to_string(),
            params: None,
            response_tx: slow_tx,
        })
        .await
        .unwrap();

    let (fast_tx, fast_rx) = oneshot::channel();
    command_tx
        .send(RuntimeCommand::Request {
            request_id: RequestId::number(11),
            method: "fast".to_string(),
            params: None,
            response_tx: fast_tx,
        })
        .await
        .unwrap();

    let fast = tokio::time::timeout(Duration::from_secs(1), fast_rx)
        .await
        .expect("fast request must complete while slow send is in flight")
        .unwrap()
        .unwrap();
    assert_eq!(fast, serde_json::json!({"ok": true}));
    assert!(slow_rx.try_recv().is_err());

    drop(command_tx);
    let _ = runtime.await;
}
