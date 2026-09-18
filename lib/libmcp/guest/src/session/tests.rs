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
}

impl CursorLoopTransport {
    fn new() -> Arc<Self> {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            incoming_tx,
            incoming_rx: Mutex::new(incoming_rx),
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
                        "capabilities": {},
                        "serverInfo": {"name": "loop-server", "version": "0.0.1"}
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [],
                        "nextCursor": "same-cursor"
                    }),
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
    assert!(matches!(error, GuestError::Protocol(_)), "got {error:?}");
    assert!(session.tools().await.is_none());

    session.disconnect().await.unwrap();
}
