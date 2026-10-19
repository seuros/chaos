use std::collections::HashMap;
use std::env;
use std::fmt::Write;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use chaos_ipc::clamp_bridge::ClampBridgeRequest;
use chaos_ipc::clamp_bridge::ClampBridgeResponse;
use chaos_ipc::mcp::Tool as BridgeToolSpec;
use chaos_ipc::models::FunctionCallOutputBody;
use chaos_ipc::models::FunctionCallOutputContentItem;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::product::OS_NAME;
use mcp_host::content::types::ContentBlock;
use mcp_host::content::types::ImageContent;
use mcp_host::content::types::TextContent;
use mcp_host::prelude::*;
use mcp_host::registry::tools::Tool;
use mcp_host::registry::tools::ToolError;
use mcp_host::registry::tools::ToolFuture;
use mcp_host::registry::tools::ToolOutput;
use mcp_host::registry::tools::ToolRegistry;
use mcp_host::server::visibility::ExecutionContext;
use mcp_host::server::visibility::VisibilityContext;
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

    let server = Server::builder("chaos-clamp-session-bridge", CHAOS_VERSION)
        .with_tools(true)
        .with_instructions(format!("{OS_NAME} session-backed tools for clamp"))
        .build();
    let state = Arc::new(BridgeState::new(
        socket_path,
        token,
        server.tool_registry().clone(),
    ));
    state.refresh().await?;

    server
        .run(StdioTransport::new())
        .await
        .map_err(|err| Error::other(format!("clamp bridge MCP server error: {err}")))
}

struct BridgeState {
    socket_path: PathBuf,
    token: String,
    registry: ToolRegistry,
    visible_tools: RwLock<HashMap<String, BridgeToolSpec>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl BridgeState {
    fn new(socket_path: PathBuf, token: String, registry: ToolRegistry) -> Self {
        Self {
            socket_path,
            token,
            registry,
            visible_tools: RwLock::new(HashMap::new()),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    async fn refresh(self: &Arc<Self>) -> IoResult<()> {
        let _guard = self.refresh_lock.lock().await;
        let tool_specs = list_tools(&self.socket_path, &self.token).await?;
        self.replace_visible_tools(tool_specs)
    }

    fn replace_visible_tools(self: &Arc<Self>, tool_specs: Vec<BridgeToolSpec>) -> IoResult<()> {
        let changed = {
            let visible = self
                .visible_tools
                .read()
                .map_err(|_| Error::other("clamp bridge visible tool state is poisoned"))?;
            tool_specs
                .iter()
                .filter(|tool| visible.get(&tool.name) != Some(*tool))
                .cloned()
                .collect::<Vec<_>>()
        };
        let next_visible = tool_specs
            .into_iter()
            .map(|tool| (tool.name.clone(), tool))
            .collect::<HashMap<_, _>>();

        for tool in changed {
            self.registry
                .register_boxed(Arc::new(BridgeTool::new(Arc::clone(self), tool)))
                .map_err(|err| Error::other(format!("invalid clamp bridge tool: {err}")))?;
        }

        let previous = std::mem::replace(
            &mut *self
                .visible_tools
                .write()
                .map_err(|_| Error::other("clamp bridge visible tool state is poisoned"))?,
            next_visible,
        );

        // Registration/removal announces changes through the host's gated
        // broadcaster. Remove stale tools instead of accumulating hidden entries.
        for name in previous.keys() {
            if !self.is_visible(name) {
                self.registry.remove(name);
            }
        }
        Ok(())
    }

    fn is_visible(&self, name: &str) -> bool {
        self.visible_tools
            .read()
            .is_ok_and(|visible| visible.contains_key(name))
    }
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
    state: Arc<BridgeState>,
    spec: BridgeToolSpec,
}

impl BridgeTool {
    fn new(state: Arc<BridgeState>, spec: BridgeToolSpec) -> Self {
        Self { state, spec }
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
        self.spec.output_schema.clone().map(|mut schema| {
            // Runtime notices are a bridge-owned, additive result field. It
            // must be declared even when the native schema forbids extras.
            if schema.get("type").and_then(Value::as_str) == Some("object")
                && let Some(root) = schema.as_object_mut()
            {
                let properties = root
                    .entry("properties")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(properties) = properties.as_object_mut() {
                    properties
                        .entry("runtime_notice")
                        .or_insert_with(|| serde_json::json!({"type":"string"}));
                }
            }
            schema
        })
    }

    fn is_visible(&self, _ctx: &VisibilityContext) -> bool {
        self.state.is_visible(&self.spec.name)
    }

    fn execute<'a>(&'a self, ctx: ExecutionContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            let response = bridge_request(
                &self.state.socket_path,
                ClampBridgeRequest::CallTool {
                    token: self.state.token.clone(),
                    name: self.spec.name.clone(),
                    arguments: ctx.params,
                },
            )
            .await
            .map_err(|err| ToolError::Execution(err.to_string()))?;
            match response {
                ClampBridgeResponse::ToolResult {
                    output,
                    structured_output,
                    notices,
                } => {
                    if matches!(self.spec.name.as_str(), "enable_tools" | "disable_tools") {
                        self.state.refresh().await.map_err(|err| {
                            ToolError::Execution(format!(
                                "tool groups changed but the clamp bridge failed to refresh: {err}"
                            ))
                        })?;
                    }
                    response_input_to_tool_output(
                        *output,
                        self.spec.output_schema.is_some(),
                        structured_output,
                    )
                    .and_then(|tool_output| with_runtime_notices(tool_output, notices))
                }
                ClampBridgeResponse::Error { message } => Err(ToolError::Execution(message)),
                ClampBridgeResponse::Tools { .. } => Err(ToolError::Internal(
                    "unexpected tool list response while executing tool".to_string(),
                )),
            }
        })
    }
}

/// Attach text steered into the running turn to the tool result the external
/// model is about to read. Structured results carry it as `runtime_notice`;
/// content results get it as a trailing text block.
fn with_runtime_notices(output: ToolOutput, notices: Vec<String>) -> Result<ToolOutput, ToolError> {
    if notices.is_empty() {
        return Ok(output);
    }
    let notice = notices.join("\n\n");
    match output {
        ToolOutput::Structured(mut map) => {
            if map.contains_key("runtime_notice") {
                return Err(ToolError::Internal(
                    "tool result collides with the bridge-owned runtime_notice field".into(),
                ));
            }
            map.insert("runtime_notice".to_string(), Value::String(notice));
            Ok(ToolOutput::Structured(map))
        }
        ToolOutput::Content(mut items) => {
            items.push(ContentBlock::Text(TextContent::new(format!(
                "[runtime notice]\n{notice}"
            ))));
            Ok(ToolOutput::Content(items))
        }
    }
}

fn response_input_to_tool_output(
    output: ResponseInputItem,
    prefer_structured: bool,
    structured_output: Option<Value>,
) -> Result<ToolOutput, ToolError> {
    match output {
        ResponseInputItem::FunctionCallOutput { output, .. }
        | ResponseInputItem::CustomToolCallOutput { output, .. } => {
            if output.success == Some(false) {
                return Err(ToolError::Execution(
                    output.body.into_text().unwrap_or_default(),
                ));
            }
            if prefer_structured {
                if let Some(value) = structured_output {
                    return ToolOutput::structured(value)
                        .map_err(|err| ToolError::Internal(err.to_string()));
                }
                let text = match &output.body {
                    FunctionCallOutputBody::Text(text) => Some(text.as_str()),
                    FunctionCallOutputBody::ContentItems(items) => match items.as_slice() {
                        [FunctionCallOutputContentItem::InputText { text }] => Some(text.as_str()),
                        _ => None,
                    },
                };
                if let Some(text) = text
                    && let Ok(Value::Object(value)) = serde_json::from_str(text)
                {
                    return Ok(ToolOutput::json(value));
                }
            }
            // A declared schema with no valid typed result remains content:
            // the SDK must reject it, not accept a fabricated wrapper.
            function_call_output_body_to_tool_output(output.body)
        }
        ResponseInputItem::McpToolCallOutput { output, .. } => {
            if output.is_error == Some(true) {
                return Err(ToolError::Execution(content_items_to_text(&output.content)));
            }
            if prefer_structured {
                // Model-visible MCP schemas describe the complete call-result
                // envelope, not only its nested structuredContent.
                return ToolOutput::structured(output)
                    .map_err(|err| ToolError::Internal(err.to_string()));
            }
            if let Some(structured) = output.structured_content {
                ToolOutput::structured(structured)
                    .map_err(|e| ToolError::Internal(format!("non-object tool output: {e}")))
            } else {
                Ok(ToolOutput::text(content_items_to_text(&output.content)))
            }
        }
        ResponseInputItem::ToolSearchOutput { tools, .. } => {
            ToolOutput::structured(serde_json::json!({ "tools": tools }))
                .map_err(|e| ToolError::Internal(format!("non-object tool output: {e}")))
        }
        ResponseInputItem::Message { content, .. } => Ok(ToolOutput::text(
            content
                .into_iter()
                .filter_map(|item| match item {
                    chaos_ipc::models::ContentItem::InputText { text }
                    | chaos_ipc::models::ContentItem::OutputText { text } => Some(text),
                    chaos_ipc::models::ContentItem::InputImage { .. } => None,
                    chaos_ipc::models::ContentItem::Document { name, text, .. } => {
                        let header = name.map(|n| format!("[{n}]\n")).unwrap_or_default();
                        Some(format!("{header}{text}"))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
    }
}

fn function_call_output_body_to_tool_output(
    body: FunctionCallOutputBody,
) -> Result<ToolOutput, ToolError> {
    let FunctionCallOutputBody::ContentItems(items) = body else {
        return Ok(ToolOutput::text(body.into_text().unwrap_or_default()));
    };

    let content = items
        .into_iter()
        .map(|item| match item {
            FunctionCallOutputContentItem::InputText { text } => {
                Ok(ContentBlock::Text(TextContent::new(text)))
            }
            FunctionCallOutputContentItem::InputImage { mut image_url, .. } => {
                let Some(data_url) = image_url.strip_prefix("data:") else {
                    return Err(ToolError::Execution(
                        "clamp bridge image output must use a base64 data URL".to_string(),
                    ));
                };
                let Some((metadata, data)) = data_url.split_once(',') else {
                    return Err(ToolError::Execution(
                        "clamp bridge image output contains an invalid data URL".to_string(),
                    ));
                };
                let Some(mime_type) = metadata.strip_suffix(";base64") else {
                    return Err(ToolError::Execution(
                        "clamp bridge image output must be base64 encoded".to_string(),
                    ));
                };
                let data_start = image_url.len() - data.len();
                let mime_type = mime_type.to_owned();
                // Remove the small URL header in place, preserving the owned
                // base64 buffer instead of allocating a second image payload.
                image_url.drain(..data_start);
                Ok(ContentBlock::Image(ImageContent::new(image_url, mime_type)))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ToolOutput::content(content))
}

fn content_items_to_text(content: &[serde_json::Value]) -> String {
    let mut text = String::new();
    for (index, item) in content.iter().enumerate() {
        if index != 0 {
            text.push('\n');
        }
        if let Some(value) = item.get("text").and_then(Value::as_str) {
            text.push_str(value);
        } else {
            let _ = write!(text, "{item}");
        }
    }
    text
}

#[cfg(test)]
mod tests;
