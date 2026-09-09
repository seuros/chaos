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
mod tests {
    use super::DynamicToolSpec;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn dynamic_tool_spec_deserializes_defer_loading() {
        let value = json!({
            "name": "lookup_ticket",
            "description": "Fetch a ticket",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                }
            },
            "deferLoading": true,
        });

        let actual: DynamicToolSpec = serde_json::from_value(value).expect("deserialize");

        assert_eq!(
            actual,
            DynamicToolSpec {
                name: "lookup_ticket".to_string(),
                description: "Fetch a ticket".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" }
                    }
                }),
                defer_loading: true,
            }
        );
    }

    #[test]
    fn dynamic_tool_spec_rejects_expose_to_context() {
        let value = json!({
            "name": "lookup_ticket",
            "description": "Fetch a ticket",
            "inputSchema": {
                "type": "object",
                "properties": {}
            },
            "exposeToContext": false,
        });

        let err = serde_json::from_value::<DynamicToolSpec>(value).expect_err("should reject");
        assert!(err.to_string().contains("exposeToContext"));
    }

    #[test]
    fn dynamic_tool_spec_defaults_to_immediate_loading() {
        let value = json!({
            "name": "lookup_ticket",
            "description": "Fetch a ticket",
            "inputSchema": {"type": "object"},
        });
        let actual: DynamicToolSpec = serde_json::from_value(value).expect("deserialize");
        assert!(!actual.defer_loading);
        let serialized = serde_json::to_value(&actual).expect("serialize");
        assert_eq!(serialized["deferLoading"], false);
        assert!(serialized.get("exposeToContext").is_none());
    }

    #[test]
    fn dynamic_tool_spec_rejects_null_defer_loading() {
        let value = json!({
            "name": "lookup_ticket",
            "description": "Fetch a ticket",
            "inputSchema": {"type": "object"},
            "deferLoading": null,
        });
        assert!(serde_json::from_value::<DynamicToolSpec>(value).is_err());
    }

    #[test]
    fn dynamic_tool_spec_rejects_mcp_only_fields() {
        let value = json!({
            "name": "lookup_ticket",
            "description": "Fetch a ticket",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                }
            },
            "outputSchema": {
                "type": "object"
            }
        });

        let err = serde_json::from_value::<DynamicToolSpec>(value).expect_err("should reject");

        assert!(
            err.to_string().contains("outputSchema"),
            "unexpected error: {err}"
        );
    }
}
