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
        output: Box<ResponseInputItem>,
        /// Optional typed native result, independent of its readable text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structured_output: Option<serde_json::Value>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{FunctionCallOutputBody, FunctionCallOutputPayload};
    use serde_json::json;

    #[test]
    fn typed_output_is_optional_and_round_trips_without_changing_native_text() {
        let output = ResponseInputItem::FunctionCallOutput {
            call_id: "exec".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("readable output".into()),
                success: Some(true),
            },
            tool_name: None,
        };
        let mut response = ClampBridgeResponse::ToolResult {
            output: Box::new(output),
            structured_output: None,
            notices: Vec::new(),
        };
        let legacy = serde_json::to_value(&response).expect("legacy wire result");
        assert!(legacy.get("structured_output").is_none());
        assert!(matches!(
            serde_json::from_value::<ClampBridgeResponse>(legacy).expect("older result"),
            ClampBridgeResponse::ToolResult {
                structured_output: None,
                ..
            }
        ));
        let ClampBridgeResponse::ToolResult {
            structured_output, ..
        } = &mut response
        else {
            unreachable!()
        };
        *structured_output = Some(json!({"wall_time_seconds": 1.25, "output": "typed output"}));
        let value = serde_json::to_value(&response).expect("typed wire result");
        let decoded: ClampBridgeResponse =
            serde_json::from_value(value).expect("typed result round trip");
        let ClampBridgeResponse::ToolResult {
            output,
            structured_output,
            ..
        } = decoded
        else {
            unreachable!()
        };
        assert_eq!(
            structured_output,
            Some(json!({"wall_time_seconds": 1.25, "output": "typed output"}))
        );
        let ResponseInputItem::FunctionCallOutput { output, .. } = *output else {
            unreachable!()
        };
        assert_eq!(output.body.to_text().as_deref(), Some("readable output"));
    }
}
