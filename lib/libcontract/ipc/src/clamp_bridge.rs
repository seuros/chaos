use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::mcp::Tool;
use crate::models::ResponseInputItem;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClampBridgeRequest {
    ListTools {
        token: String,
    },
    CallTool {
        token: String,
        name: String,
        arguments: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClampBridgeResponse {
    Tools { tools: Vec<Tool> },
    ToolResult { output: ResponseInputItem },
    Error { message: String },
}
