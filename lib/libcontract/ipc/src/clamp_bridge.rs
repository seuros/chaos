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
    Tools {
        tools: Vec<Tool>,
    },
    ToolResult {
        output: ResponseInputItem,
        /// Text steered into the running turn since the last tool call. Under
        /// clamp the whole external run is one kernel sample, so a steer would
        /// otherwise wait for the turn to end; it rides on this tool result.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notices: Vec<String>,
    },
    Error {
        message: String,
    },
}
