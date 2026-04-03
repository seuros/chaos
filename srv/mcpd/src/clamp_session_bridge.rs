use std::env;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use chaos_ipc::clamp_bridge::ClampBridgeRequest;
use chaos_ipc::clamp_bridge::ClampBridgeResponse;
use chaos_ipc::mcp::Tool as BridgeToolSpec;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::product::CHAOS_VERSION;
use mcp_host::prelude::*;
use mcp_host::registry::tools::Tool;
use mcp_host::registry::tools::ToolError;
use mcp_host::registry::tools::ToolFuture;
use mcp_host::registry::tools::ToolOutput;
use mcp_host::server::visibility::ExecutionContext;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixStream;

const SOCKET_ENV: &str = "CHAOS_CLAMP_MCP_SOCKET";
const TOKEN_ENV: &str = "CHAOS_CLAMP_MCP_TOKEN";

pub async fn run_main() -> IoResult<()> {
    let socket_path = env::var_os(SOCKET_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("missing {SOCKET_ENV}")))?;
    let token = env::var(TOKEN_ENV)
        .map_err(|_| Error::new(ErrorKind::InvalidInput, format!("missing {TOKEN_ENV}")))?;

    let tool_specs = list_tools(&socket_path, &token).await?;
    let server = server("chaos-clamp-session-bridge", CHAOS_VERSION)
        .with_tools(true)
        .with_instructions("Chaos session-backed tools for clamp")
        .build();

    for tool in tool_specs {
        server
            .tool_registry()
            .register_boxed(Arc::new(BridgeTool::new(
                socket_path.clone(),
                token.clone(),
                tool,
            )));
    }

    server
        .run(StdioTransport::new())
        .await
        .map_err(|err| Error::other(format!("clamp bridge MCP server error: {err}")))
}

async fn list_tools(socket_path: &Path, token: &str) -> IoResult<Vec<BridgeToolSpec>> {
    match bridge_request(
        socket_path,
        ClampBridgeRequest::ListTools {
            token: token.to_string(),
        },
    )
    .await?
    {
        ClampBridgeResponse::Tools { tools } => Ok(tools),
        ClampBridgeResponse::Error { message } => Err(Error::other(message)),
        ClampBridgeResponse::ToolResult { .. } => Err(Error::new(
            ErrorKind::InvalidData,
            "unexpected tool_result while listing clamp bridge tools",
        )),
    }
}

async fn bridge_request(
    socket_path: &Path,
    request: ClampBridgeRequest,
) -> IoResult<ClampBridgeResponse> {
    let stream = UnixStream::connect(socket_path).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_vec(&request)?;
    payload.push(b'\n');
    write_half.write_all(&payload).await?;
    write_half.flush().await?;

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    serde_json::from_str::<ClampBridgeResponse>(&line)
        .map_err(|err| Error::new(ErrorKind::InvalidData, err))
}

#[derive(Clone)]
struct BridgeTool {
    socket_path: PathBuf,
    token: String,
    spec: BridgeToolSpec,
}

impl BridgeTool {
    fn new(socket_path: PathBuf, token: String, spec: BridgeToolSpec) -> Self {
        Self {
            socket_path,
            token,
            spec,
        }
    }
}

impl Tool for BridgeTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn title(&self) -> Option<&str> {
        self.spec.title.as_deref()
    }

    fn description(&self) -> Option<&str> {
        self.spec.description.as_deref()
    }

    fn input_schema(&self) -> Value {
        self.spec.input_schema.clone()
    }

    fn output_schema(&self) -> Option<Value> {
        self.spec.output_schema.clone()
    }

    fn execute<'a>(&'a self, ctx: ExecutionContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            let response = bridge_request(
                &self.socket_path,
                ClampBridgeRequest::CallTool {
                    token: self.token.clone(),
                    name: self.spec.name.clone(),
                    arguments: ctx.params.clone(),
                },
            )
            .await
            .map_err(|err| ToolError::Execution(err.to_string()))?;
            match response {
                ClampBridgeResponse::ToolResult { output } => {
                    response_input_to_tool_output(output, self.spec.output_schema.is_some())
                }
                ClampBridgeResponse::Error { message } => Err(ToolError::Execution(message)),
                ClampBridgeResponse::Tools { .. } => Err(ToolError::Internal(
                    "unexpected tool list response while executing tool".to_string(),
                )),
            }
        })
    }
}

fn response_input_to_tool_output(
    output: ResponseInputItem,
    prefer_structured: bool,
) -> Result<ToolOutput, ToolError> {
    match output {
        ResponseInputItem::FunctionCallOutput { output, .. }
        | ResponseInputItem::CustomToolCallOutput { output, .. } => {
            if prefer_structured {
                ToolOutput::structured(serde_json::json!({
                    "output": output.body.to_text().unwrap_or_default(),
                    "success": output.success
                }))
                .map_err(|err| ToolError::Internal(err.to_string()))
            } else {
                Ok(ToolOutput::text(output.body.to_text().unwrap_or_default()))
            }
        }
        ResponseInputItem::McpToolCallOutput { output, .. } => {
            if let Some(structured) = output.structured_content {
                Ok(ToolOutput::json(structured))
            } else {
                Ok(ToolOutput::text(content_items_to_text(&output.content)))
            }
        }
        ResponseInputItem::ToolSearchOutput { tools, .. } => {
            Ok(ToolOutput::json(serde_json::json!({ "tools": tools })))
        }
        ResponseInputItem::Message { content, .. } => Ok(ToolOutput::text(
            content
                .into_iter()
                .filter_map(|item| match item {
                    chaos_ipc::models::ContentItem::InputText { text }
                    | chaos_ipc::models::ContentItem::OutputText { text } => Some(text),
                    chaos_ipc::models::ContentItem::InputImage { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
    }
}

fn content_items_to_text(content: &[serde_json::Value]) -> String {
    content
        .iter()
        .map(|item| {
            item.get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| serde_json::to_string(item).unwrap_or_default())
        })
        .collect::<Vec<_>>()
        .join("\n")
}
