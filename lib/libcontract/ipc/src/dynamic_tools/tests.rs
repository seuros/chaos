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
