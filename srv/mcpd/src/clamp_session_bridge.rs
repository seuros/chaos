use std::collections::HashMap;
use std::env;
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
use mcp_host::content::types::Content;
use mcp_host::content::types::ImageContent;
use mcp_host::content::types::TextContent;
use mcp_host::prelude::*;
use mcp_host::registry::tools::Tool;
use mcp_host::registry::tools::ToolError;
use mcp_host::registry::tools::ToolFuture;
use mcp_host::registry::tools::ToolOutput;
use mcp_host::registry::tools::ToolRegistry;
use mcp_host::server::NotificationSender;
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
        server.notification_sender(),
    ));
    state.refresh(false).await?;

    server
        .run(StdioTransport::new())
        .await
        .map_err(|err| Error::other(format!("clamp bridge MCP server error: {err}")))
}

struct BridgeState {
    socket_path: PathBuf,
    token: String,
    registry: ToolRegistry,
    notification_sender: NotificationSender,
    visible_tools: RwLock<HashMap<String, BridgeToolSpec>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl BridgeState {
    fn new(
        socket_path: PathBuf,
        token: String,
        registry: ToolRegistry,
        notification_sender: NotificationSender,
    ) -> Self {
        Self {
            socket_path,
            token,
            registry,
            notification_sender,
            visible_tools: RwLock::new(HashMap::new()),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    async fn refresh(self: &Arc<Self>, notify: bool) -> IoResult<()> {
        let _guard = self.refresh_lock.lock().await;
        let tool_specs = list_tools(&self.socket_path, &self.token).await?;
        self.replace_visible_tools(tool_specs, notify)
    }

    fn replace_visible_tools(
        self: &Arc<Self>,
        tool_specs: Vec<BridgeToolSpec>,
        notify: bool,
    ) -> IoResult<()> {
        let next_visible = tool_specs
            .iter()
            .map(|tool| (tool.name.clone(), tool.clone()))
            .collect::<HashMap<_, _>>();
        let changed = {
            let visible = self
                .visible_tools
                .read()
                .map_err(|_| Error::other("clamp bridge visible tool state is poisoned"))?;
            *visible != next_visible
        };

        for tool in tool_specs {
            self.registry
                .try_register_boxed(Arc::new(BridgeTool::new(Arc::clone(self), tool)))
                .map_err(|err| Error::other(format!("invalid clamp bridge tool: {err}")))?;
        }

        *self
            .visible_tools
            .write()
            .map_err(|_| Error::other("clamp bridge visible tool state is poisoned"))? =
            next_visible;

        if notify && changed {
            let _ = self.notification_sender.send(JsonRpcNotification::new(
                "notifications/tools/list_changed",
                None,
            ));
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
        self.spec.output_schema.clone()
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
                    arguments: ctx.params.clone(),
                },
            )
            .await
            .map_err(|err| ToolError::Execution(err.to_string()))?;
            match response {
                ClampBridgeResponse::ToolResult { output } => {
                    if matches!(self.spec.name.as_str(), "enable_tools" | "disable_tools") {
                        self.state.refresh(true).await.map_err(|err| {
                            ToolError::Execution(format!(
                                "tool groups changed but the clamp bridge failed to refresh: {err}"
                            ))
                        })?;
                    }
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
                function_call_output_body_to_tool_output(output.body)
            }
        }
        ResponseInputItem::McpToolCallOutput { output, .. } => {
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
        return Ok(ToolOutput::text(body.to_text().unwrap_or_default()));
    };

    let content = items
        .into_iter()
        .map(|item| match item {
            FunctionCallOutputContentItem::InputText { text } => {
                Ok(Box::new(TextContent::new(text)) as Box<dyn Content>)
            }
            FunctionCallOutputContentItem::InputImage { image_url, .. } => {
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
                Ok(Box::new(ImageContent::new(data, mime_type.to_string())) as Box<dyn Content>)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ToolOutput::content(content))
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

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::models::FunctionCallOutputPayload;

    fn tool_spec(name: &str) -> BridgeToolSpec {
        BridgeToolSpec {
            name: name.to_string(),
            title: None,
            description: None,
            input_schema: serde_json::json!({ "type": "object" }),
            output_schema: None,
            annotations: None,
            icons: None,
            meta: None,
        }
    }

    #[test]
    fn clamp_bridge_replaces_visible_tools_after_group_changes() {
        let registry = ToolRegistry::new();
        let (notification_sender, mut notification_rx) = NotificationSender::bounded(4);
        let state = Arc::new(BridgeState::new(
            PathBuf::from("/unused"),
            "token".to_string(),
            registry.clone(),
            notification_sender,
        ));

        state
            .replace_visible_tools(vec![tool_spec("enable_tools")], false)
            .expect("initial tool list");
        assert!(state.is_visible("enable_tools"));
        assert!(!state.is_visible("exec_command"));
        assert!(notification_rx.try_recv().is_err());

        state
            .replace_visible_tools(vec![tool_spec("exec_command")], true)
            .expect("refreshed tool list");
        assert!(!state.is_visible("enable_tools"));
        assert!(state.is_visible("exec_command"));
        assert!(registry.get("enable_tools").is_some());
        assert!(registry.get("exec_command").is_some());

        let notification = notification_rx
            .try_recv()
            .expect("tool list changed notification");
        assert_eq!(notification.method, "notifications/tools/list_changed");
    }

    #[test]
    fn clamp_bridge_preserves_image_only_function_output() {
        let output = ResponseInputItem::FunctionCallOutput {
            call_id: "view-image".to_string(),
            output: FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,Zm9v".to_string(),
                    detail: None,
                },
            ]),
            tool_name: Some("view_image".to_string()),
        };

        let response = response_input_to_tool_output(output, false)
            .expect("image output should convert")
            .into_response_value();

        assert_eq!(
            response,
            serde_json::json!({
                "content": [{
                    "type": "image",
                    "data": "Zm9v",
                    "mimeType": "image/png"
                }]
            })
        );
    }

    #[test]
    fn clamp_bridge_preserves_mixed_text_and_image_function_output() {
        let output = ResponseInputItem::FunctionCallOutput {
            call_id: "mixed-output".to_string(),
            output: FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "preview".to_string(),
                },
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/webp;base64,Zm9v".to_string(),
                    detail: None,
                },
            ]),
            tool_name: None,
        };

        let response = response_input_to_tool_output(output, false)
            .expect("mixed output should convert")
            .into_response_value();

        assert_eq!(
            response,
            serde_json::json!({
                "content": [
                    {
                        "type": "text",
                        "text": "preview"
                    },
                    {
                        "type": "image",
                        "data": "Zm9v",
                        "mimeType": "image/webp"
                    }
                ]
            })
        );
    }
}
