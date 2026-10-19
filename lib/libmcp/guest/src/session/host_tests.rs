//! JSON round trips against the actual host dispatcher, without a CLI build.

use super::*;
use crate::handler::NoopClientHandler;
use crate::runtime::{ConnectionOptions, connect_with_transport};
use crate::transport::TransportFuture;
use mcp_host::prelude::{ExecutionContext, Server, ToolError, ToolOutput};
use mcp_host::protocol::types::{TaskSupport, ToolExecution};
use mcp_host::registry::tools::{Tool, ToolFuture};
use serde_json::json;
use std::sync::atomic::AtomicUsize;
use tokio::sync::Mutex;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct HostTransport {
    server: Server,
    incoming: Mutex<mpsc::Receiver<crate::protocol::JsonRpcMessage>>,
    responses: mpsc::Sender<crate::protocol::JsonRpcMessage>,
}

struct EchoTool(Arc<AtomicUsize>);

fn echo_schema() -> Value {
    json!({
        "type":"object", "properties":{"value":{"type":"string"}},
        "required":["value"], "additionalProperties":false
    })
}

impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn input_schema(&self) -> Value {
        echo_schema()
    }
    fn output_schema(&self) -> Option<Value> {
        Some(echo_schema())
    }
    fn execution(&self) -> Option<ToolExecution> {
        Some(ToolExecution {
            task_support: Some(TaskSupport::Optional),
        })
    }
    fn execute<'a>(&'a self, ctx: ExecutionContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            ToolOutput::structured(ctx.params)
                .map_err(|error| ToolError::Execution(error.to_string()))
        })
    }
}

impl HostTransport {
    fn new() -> TestResult<(Arc<Self>, Arc<AtomicUsize>)> {
        let server = Server::builder("round-trip", "1")
            .with_tools(true)
            .with_tasks(true, true)
            .build();
        let calls = Arc::new(AtomicUsize::new(0));
        server.tool_registry().register(EchoTool(calls.clone()))?;
        let (responses, incoming) = mpsc::channel(8);
        Ok((
            Arc::new(Self {
                server,
                incoming: Mutex::new(incoming),
                responses,
            }),
            calls,
        ))
    }
}

impl MessageTransport for HostTransport {
    fn send<'a>(&'a self, message: crate::protocol::JsonRpcMessage) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            // Exercise owned wire decoding, not just shared Rust type identity.
            let message = serde_json::from_slice(&serde_json::to_vec(&message)?)?;
            let request = match message {
                crate::protocol::JsonRpcMessage::Request(request)
                | crate::protocol::JsonRpcMessage::Notification(request) => request,
                crate::protocol::JsonRpcMessage::Response(_) => {
                    return Err(GuestError::Protocol("unexpected client response".into()));
                }
            };
            if let Some(response) = self.server.handle_request("guest", request).await {
                let message = serde_json::from_slice(&serde_json::to_vec(&response)?)?;
                self.responses
                    .send(message)
                    .await
                    .map_err(|_| GuestError::Disconnected)?;
            }
            Ok(())
        })
    }

    fn recv<'a>(&'a self) -> TransportFuture<'a, crate::protocol::JsonRpcMessage> {
        Box::pin(async move {
            self.incoming
                .lock()
                .await
                .recv()
                .await
                .ok_or(GuestError::Disconnected)
        })
    }

    fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            self.server.remove_session("guest");
            Ok(())
        })
    }
}

#[tokio::test]
async fn host_schema_validation_and_task_results_round_trip_through_guest() -> TestResult {
    let (transport, calls) = HostTransport::new()?;
    let session = connect_with_transport(
        transport,
        ConnectionOptions {
            client_info: crate::protocol::Implementation::new("guest", "1"),
            capabilities: crate::protocol::ClientCapabilities::default(),
            handler: Arc::new(NoopClientHandler),
            default_timeout: Duration::from_secs(5),
        },
    )
    .await?;
    assert_eq!(session.server_info().protocol_version, "2025-11-25");
    assert_eq!(session.list_tools().await?.len(), 1);
    let invalid = session
        .call_tool("echo", Some(serde_json::from_value(json!({"value":42}))?))
        .await;
    assert!(match invalid {
        Err(_) => true,
        Ok(response) => serde_json::to_value(response)?["isError"] == true,
    });
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "invalid args reached the handler"
    );

    let reply = session
        .call_tool_with(CallToolRequestParams {
            name: "echo".into(),
            arguments: Some(serde_json::from_value(json!({"value":"task-result"}))?),
            meta: None,
            task: Some(serde_json::from_value(json!({"ttl":30000}))?),
        })
        .await?;
    let task = reply.into_task().ok_or("task envelope expected")?;
    let result = session.task_result(task.task.task_id).await?;
    assert_eq!(result["structuredContent"], json!({"value":"task-result"}));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    session.disconnect().await?;
    Ok(())
}
