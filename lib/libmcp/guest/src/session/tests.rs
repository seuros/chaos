use super::*;
use crate::handler::NoopClientHandler;
use crate::protocol::ClientCapabilities;
use crate::protocol::Implementation;
use crate::protocol::JsonRpcMessage;
use crate::protocol::JsonRpcResponse;
use crate::runtime::ConnectionOptions;
use crate::runtime::connect_with_transport;
use crate::transport::TransportFuture;
use tokio::sync::Mutex;

struct CursorLoopTransport {
    incoming_tx: mpsc::UnboundedSender<JsonRpcMessage>,
    incoming_rx: Mutex<mpsc::UnboundedReceiver<JsonRpcMessage>>,
    logging_requests: Mutex<Vec<Value>>,
}

impl CursorLoopTransport {
    fn new() -> Arc<Self> {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            incoming_tx,
            incoming_rx: Mutex::new(incoming_rx),
            logging_requests: Mutex::new(Vec::new()),
        })
    }
}

impl MessageTransport for CursorLoopTransport {
    fn send<'a>(&'a self, message: JsonRpcMessage) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            if let JsonRpcMessage::Request(request) = &message {
                let id = request.id.clone().unwrap();
                let result = match request.method.as_str() {
                    "initialize" => serde_json::json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"logging": {}},
                        "serverInfo": {"name": "loop-server", "version": "0.0.1"}
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [],
                        "nextCursor": "same-cursor"
                    }),
                    "logging/setLevel" => {
                        self.logging_requests
                            .lock()
                            .await
                            .push(request.params.clone().unwrap());
                        serde_json::json!({})
                    }
                    other => serde_json::json!({"unexpected": other}),
                };
                let _ = self
                    .incoming_tx
                    .send(JsonRpcMessage::Response(JsonRpcResponse::success(
                        id, result,
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
async fn logging_levels_use_typed_wire_names_and_reject_invalid_values_locally() {
    let transport = CursorLoopTransport::new();
    let session = connect_with_transport(
        transport.clone(),
        ConnectionOptions {
            client_info: Implementation::new("test-client", "1.0.0"),
            capabilities: ClientCapabilities::default(),
            handler: Arc::new(NoopClientHandler),
            default_timeout: Duration::from_secs(5),
        },
    )
    .await
    .unwrap();

    let levels = [
        "emergency",
        "alert",
        "critical",
        "error",
        "warning",
        "notice",
        "info",
        "debug",
    ];
    for level in levels {
        session.set_logging_level(level).await.unwrap();
    }
    // Preserve compatibility with owned strings as well as borrowed strings.
    session.set_logging_level("info".to_owned()).await.unwrap();
    let expected: Vec<_> = levels
        .into_iter()
        .chain(["info"])
        .map(|level| serde_json::json!({"level": level}))
        .collect();
    assert_eq!(*transport.logging_requests.lock().await, expected);

    for invalid in ["", "warn", "INFO", " info", "warning "] {
        assert!(matches!(
            session.set_logging_level(invalid).await,
            Err(GuestError::InvalidParams(_))
        ));
    }
    assert_eq!(*transport.logging_requests.lock().await, expected);
    // Invalid input must neither send a request nor disconnect a healthy session.
    session.set_logging_level("debug").await.unwrap();
    assert_eq!(
        transport.logging_requests.lock().await.len(),
        expected.len() + 1
    );
    session.disconnect().await.unwrap();
}

#[tokio::test]
async fn list_tools_rejects_a_server_that_repeats_cursors() {
    let session = connect_with_transport(
        CursorLoopTransport::new(),
        ConnectionOptions {
            client_info: Implementation::new("test-client", "1.0.0"),
            capabilities: ClientCapabilities::default(),
            handler: Arc::new(NoopClientHandler),
            default_timeout: Duration::from_secs(5),
        },
    )
    .await
    .unwrap();

    let error = session.list_tools().await.unwrap_err();
    std::assert_matches!(error, GuestError::Protocol(_), "got {error:?}");
    assert!(session.tools().await.is_none());

    session.disconnect().await.unwrap();
    session.disconnect().await.unwrap();
    assert!(matches!(
        session.request_value("tools/list", None).await,
        Err(GuestError::Disconnected)
    ));
}
