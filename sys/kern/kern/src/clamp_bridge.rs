use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;

use chaos_ipc::clamp_bridge::ClampBridgeRequest;
use chaos_ipc::clamp_bridge::ClampBridgeResponse;
use chaos_ipc::mcp::Tool as McpTool;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::ResponseItem;
use rand::distr::Alphanumeric;
use rand::distr::SampleString;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::warn;

use crate::chaos::built_tools;
use crate::client::active_clamp_turn_context;
use crate::client_common::tools::FreeformTool;
use crate::client_common::tools::ResponsesApiTool;
use crate::client_common::tools::ToolSpec;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::router::ToolCallSource;
use crate::tools::router::ToolRouter;
use crate::turn_diff_tracker::TurnDiffTracker;

#[derive(Debug)]
pub(crate) struct ClampSessionBridge {
    socket_path: PathBuf,
    token: String,
    task: JoinHandle<()>,
}

impl ClampSessionBridge {
    pub(crate) async fn spawn(session: Weak<crate::chaos::Session>) -> io::Result<Self> {
        let socket_path = new_socket_path();
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = UnixListener::bind(&socket_path)?;
        let token = new_token();
        let task_socket_path = socket_path.clone();
        let task_token = token.clone();
        let task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let session = session.clone();
                        let token = task_token.clone();
                        tokio::spawn(async move {
                            if let Err(err) = handle_stream(stream, session, &token).await {
                                debug!("clamp bridge stream failed: {err}");
                            }
                        });
                    }
                    Err(err) => {
                        warn!("clamp bridge accept failed: {err}");
                        break;
                    }
                }
            }
            let _ = std::fs::remove_file(&task_socket_path);
        });
        Ok(Self {
            socket_path,
            token,
            task,
        })
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) async fn shutdown(self) -> io::Result<()> {
        self.task.abort();
        let _ = self.task.await;
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }
        Ok(())
    }
}

fn new_socket_path() -> PathBuf {
    let random = Alphanumeric.sample_string(&mut rand::rng(), 24);
    std::env::temp_dir().join(format!("chaos-clamp-mcp-{random}.sock"))
}

fn new_token() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 40)
}

async fn handle_stream(
    stream: UnixStream,
    session: Weak<crate::chaos::Session>,
    token: &str,
) -> io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.trim().is_empty() {
        return Ok(());
    }

    let response = match serde_json::from_str::<ClampBridgeRequest>(&line) {
        Ok(request) => handle_request(session, token, request).await,
        Err(err) => ClampBridgeResponse::Error {
            message: format!("invalid clamp bridge request: {err}"),
        },
    };
    let mut payload = serde_json::to_vec(&response)?;
    payload.push(b'\n');
    write_half.write_all(&payload).await?;
    write_half.flush().await?;
    Ok(())
}

async fn handle_request(
    session: Weak<crate::chaos::Session>,
    token: &str,
    request: ClampBridgeRequest,
) -> ClampBridgeResponse {
    let (provided_token, request_kind) = match request {
        ClampBridgeRequest::ListTools { token } => (token, ClampRequestKind::ListTools),
        ClampBridgeRequest::CallTool {
            token,
            name,
            arguments,
        } => (token, ClampRequestKind::CallTool { name, arguments }),
    };

    if provided_token != token {
        return ClampBridgeResponse::Error {
            message: "invalid clamp bridge token".to_string(),
        };
    }

    let Some(session) = session.upgrade() else {
        return ClampBridgeResponse::Error {
            message: "session closed".to_string(),
        };
    };
    let Some(turn_context) = active_clamp_turn_context(&session).await else {
        return ClampBridgeResponse::Error {
            message: "no active turn for clamp bridge".to_string(),
        };
    };

    let cancellation = CancellationToken::new();
    let router = match built_tools(&session, &turn_context, &[], &cancellation).await {
        Ok(router) => router,
        Err(err) => {
            return ClampBridgeResponse::Error {
                message: format!("failed to build clamp tools: {err}"),
            };
        }
    };

    match request_kind {
        ClampRequestKind::ListTools => ClampBridgeResponse::Tools {
            tools: router
                .model_visible_specs()
                .into_iter()
                .filter_map(tool_spec_to_mcp_tool)
                .collect(),
        },
        ClampRequestKind::CallTool { name, arguments } => {
            match dispatch_tool_call(Arc::clone(&session), turn_context, router, &name, arguments)
                .await
            {
                Ok((output, structured_output)) => ClampBridgeResponse::ToolResult {
                    output: Box::new(output),
                    structured_output,
                    notices: take_steered_notices(&session).await,
                },
                Err(err) => ClampBridgeResponse::Error { message: err },
            }
        }
    }
}

/// Drain text steered into the active turn, and the host notice if it is still
/// valid, so they reach the external model on
/// this tool result. Anything that is not steered user text goes back on the
/// turn's pending queue untouched.
async fn take_steered_notices(session: &Arc<crate::chaos::Session>) -> Vec<String> {
    let (mut notices, other) = split_steered_notices(session.get_pending_input().await);
    if let Some(text) = session.host_notice.take(crate::host_notice::now_unix()) {
        notices.push(text);
    }
    if !other.is_empty()
        && let Err(items) = session.inject_response_items(other).await
    {
        warn!(
            "dropped {} pending clamp items: turn ended during tool call",
            items.len()
        );
    }
    notices
}

pub(crate) fn split_steered_notices(
    pending: Vec<chaos_ipc::models::ResponseInputItem>,
) -> (Vec<String>, Vec<chaos_ipc::models::ResponseInputItem>) {
    use chaos_ipc::models::ContentItem;
    use chaos_ipc::models::ResponseInputItem;

    let mut notices = Vec::new();
    let mut other = Vec::new();
    for item in pending {
        match item {
            ResponseInputItem::Message { role, content } if role == "user" => {
                let text = content
                    .iter()
                    .filter_map(|part| match part {
                        ContentItem::InputText { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.trim().is_empty() {
                    other.push(ResponseInputItem::Message { role, content });
                } else {
                    notices.push(text);
                }
            }
            item => other.push(item),
        }
    }
    (notices, other)
}

enum ClampRequestKind {
    ListTools,
    CallTool {
        name: String,
        arguments: serde_json::Value,
    },
}

/// The native CLI's lazy tool catalogue is in its discarded system prompt.
/// Render the current kernel-owned MCP projection beside canonical instructions.
pub(crate) fn antigravity_system_prompt(prompt: &crate::client_common::Prompt) -> String {
    let tools: Vec<McpTool> = prompt
        .tools
        .iter()
        .cloned()
        .filter_map(tool_spec_to_mcp_tool)
        .collect();
    let tools = serde_json::json!(tools);
    let base = &prompt.base_instructions.text;
    format!(
        "{base}\n\n# Chaos MCP tool catalogue\n\n\
         The following JSON declares the available Chaos tools and their inputSchema. \
         Invoke them using call_mcp_tool with ServerName \"chaos\", ToolName equal to \
         the exact declared name, and Arguments matching that tool's inputSchema. \
         Do not guess tool names or use native CLI command/file tools instead. \
         Tool availability is not permission: all calls remain subject to Chaos policy.\n\n\
         {tools}"
    )
}

fn tool_spec_to_mcp_tool(spec: ToolSpec) -> Option<McpTool> {
    match spec {
        ToolSpec::Function(tool) => Some(function_tool_to_mcp(tool)),
        ToolSpec::Freeform(tool) => Some(freeform_tool_to_mcp(tool)),
        ToolSpec::ToolSearch { .. }
        | ToolSpec::LocalShell {}
        | ToolSpec::ImageGeneration { .. }
        | ToolSpec::WebSearch { .. } => None,
    }
}

fn function_tool_to_mcp(tool: ResponsesApiTool) -> McpTool {
    McpTool {
        name: tool.name,
        title: None,
        description: Some(tool.description),
        input_schema: serde_json::Value::from(&tool.parameters),
        output_schema: tool.output_schema,
        annotations: None,
        icons: None,
        meta: None,
    }
}

fn freeform_tool_to_mcp(tool: FreeformTool) -> McpTool {
    McpTool {
        name: tool.name,
        title: None,
        description: Some(tool.description),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": format!(
                        "Freeform {} input (syntax: {}).",
                        tool.format.r#type,
                        tool.format.syntax
                    )
                }
            },
            "required": ["input"],
            "additionalProperties": false
        }),
        output_schema: None,
        annotations: None,
        icons: None,
        meta: None,
    }
}

async fn dispatch_tool_call(
    session: Arc<crate::chaos::Session>,
    turn_context: Arc<crate::chaos::TurnContext>,
    router: Arc<ToolRouter>,
    name: &str,
    arguments: serde_json::Value,
) -> Result<(ResponseInputItem, Option<serde_json::Value>), String> {
    let specs_by_name: HashMap<String, ToolSpec> = router
        .model_visible_specs()
        .into_iter()
        .map(|spec| (spec.name().to_string(), spec))
        .collect();
    let Some(spec) = specs_by_name.get(name) else {
        return Err(format!("clamp bridge tool not found: {name}"));
    };

    let call_id = format!("clamp_bridge_{}", uuid::Uuid::now_v7());
    let item = match spec {
        ToolSpec::Function(_) => ResponseItem::FunctionCall {
            id: None,
            name: name.to_string(),
            namespace: None,
            arguments: serde_json::to_string(&arguments)
                .map_err(|err| format!("failed to encode tool arguments: {err}"))?,
            call_id,
            provider_metadata: None,
        },
        ToolSpec::Freeform(_) => ResponseItem::CustomToolCall {
            id: None,
            status: None,
            call_id,
            name: name.to_string(),
            input: arguments
                .get("input")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("freeform clamp bridge tool '{name}' requires input"))?
                .to_string(),
        },
        _ => return Err(format!("tool '{name}' cannot be bridged through clamp MCP")),
    };

    let call = ToolRouter::build_tool_call(&session, item)
        .await
        .map_err(|err| format!("failed to build tool call: {err}"))?
        .ok_or_else(|| format!("tool '{name}' could not be built for dispatch"))?;
    let tracker: SharedTurnDiffTracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    let output = router
        .dispatch_tool_call(session, turn_context, tracker, call, ToolCallSource::Direct)
        .await
        .map_err(|err| format!("tool execution failed: {err}"))?;
    let structured_output = output.result.structured_output();
    Ok((output.into_response(), structured_output))
}

#[cfg(test)]
mod tests;
