use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

/// Session-scoped local tool registration used by Chaos host apps.
///
/// This is intentionally *not* an MCP `Tool` definition: there is no MCP
/// initialize handshake, no `tools/list`, and no `notifications/tools/list_changed`
/// lifecycle attached to this type. Callers that need negotiated MCP semantics
/// should expose a real MCP server instead of sending extra MCP-only fields here.
///
/// Tool visibility uses `deferLoading` (default false). The retired
/// `exposeToContext` field is rejected, along with all other unknown fields.
/// Saved session metadata handles its historical wire format separately.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
    #[serde(default)]
    pub defer_loading: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallRequest {
    pub call_id: String,
    pub turn_id: String,
    pub tool: String,
    pub arguments: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolResponse {
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DynamicToolCallOutputContentItem {
    #[serde(rename_all = "camelCase")]
    InputText { text: String },
    #[serde(rename_all = "camelCase")]
    InputImage { image_url: String },
}

#[cfg(test)]
mod tests;
