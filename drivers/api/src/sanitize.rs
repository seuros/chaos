//! OpenAI-specific JSON Schema sanitization.
//!
//! The OpenAI Responses API accepts a limited subset of JSON Schema for tool
//! parameters. This module provides the `JsonSchema` enum that models those
//! constraints and a `sanitize_json_schema` function that coerces arbitrary
//! JSON Schema values into the supported subset.
//!
//! This code lives in the API driver — not the kernel — because it is
//! provider-specific. Other providers (Anthropic, etc.) may accept richer
//! schemas and should not be penalized by OpenAI's limitations.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{self, Value as JsonValue, json};

/// Generic JSON-Schema subset accepted by the OpenAI Responses API.
///
/// Does NOT support `$ref`, `$defs`, `anyOf`, `oneOf`, or union types.
/// Use [`sanitize_json_schema`] to coerce richer schemas into this subset
/// before deserialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum JsonSchema {
    Boolean {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    String {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// MCP schema allows "number" | "integer" for Number
    #[serde(alias = "integer")]
    Number {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Array {
        items: Box<JsonSchema>,

        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Object {
        properties: BTreeMap<String, JsonSchema>,
        #[serde(skip_serializing_if = "Option::is_none")]
        required: Option<Vec<String>>,
        #[serde(
            rename = "additionalProperties",
            skip_serializing_if = "Option::is_none"
        )]
        additional_properties: Option<AdditionalProperties>,
    },
}

/// Whether additional properties are allowed, and if so, any required schema
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AdditionalProperties {
    Boolean(bool),
    Schema(Box<JsonSchema>),
}

impl From<bool> for AdditionalProperties {
    fn from(b: bool) -> Self {
        Self::Boolean(b)
    }
}

impl From<JsonSchema> for AdditionalProperties {
    fn from(s: JsonSchema) -> Self {
        Self::Schema(Box::new(s))
    }
}

/// Parse a raw JSON Schema value into the OpenAI-compatible [`JsonSchema`]
/// enum, sanitizing it first.
pub fn parse_tool_input_schema(input_schema: &JsonValue) -> Result<JsonSchema, serde_json::Error> {
    let mut input_schema = input_schema.clone();
    sanitize_json_schema(&mut input_schema);
    serde_json::from_value::<JsonSchema>(input_schema)
}

/// An OpenAI Responses API function tool definition.
///
/// When serialized, produces the `{"type": "function", "name": ..., "parameters": ...}`
/// shape expected by the Responses API.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResponsesApiTool {
    pub name: String,
    pub description: String,
    pub strict: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    pub parameters: JsonSchema,
    #[serde(skip)]
    pub output_schema: Option<JsonValue>,
}

/// Wraps an MCP tool's structured output schema in the OpenAI call-tool
/// result envelope (`content`, `structuredContent`, `isError`, `_meta`).
pub fn mcp_call_tool_result_output_schema(structured_content_schema: JsonValue) -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "content": {
                "type": "array",
                "items": {}
            },
            "structuredContent": structured_content_schema,
            "isError": {
                "type": "boolean"
            },
            "_meta": {}
        },
        "required": ["content"],
        "additionalProperties": false
    })
}

/// Sanitize a JSON Schema (as `serde_json::Value`) so it fits the limited
/// [`JsonSchema`] enum accepted by OpenAI.
///
/// This function:
/// - Ensures every schema object has a `"type"`. If missing, infers it from
///   common keywords (`properties` → object, `items` → array, `enum`/`const`/`format` → string)
///   and otherwise defaults to `"string"`.
/// - Normalizes union types (`["string", "null"]`) to a single type.
/// - Fills required child fields (e.g. array items, object properties) with
///   permissive defaults when absent.
pub fn sanitize_json_schema(value: &mut JsonValue) {
    match value {
        JsonValue::Bool(_) => {
            // JSON Schema boolean form: true/false. Coerce to an accept-all string.
            *value = json!({ "type": "string" });
        }
        JsonValue::Array(arr) => {
            for v in arr.iter_mut() {
                sanitize_json_schema(v);
            }
        }
        JsonValue::Object(map) => {
            // First, recursively sanitize known nested schema holders
            if let Some(props) = map.get_mut("properties")
                && let Some(props_map) = props.as_object_mut()
            {
                for (_k, v) in props_map.iter_mut() {
                    sanitize_json_schema(v);
                }
            }
            if let Some(items) = map.get_mut("items") {
                sanitize_json_schema(items);
            }
            // Some schemas use oneOf/anyOf/allOf - sanitize their entries
            for combiner in ["oneOf", "anyOf", "allOf", "prefixItems"] {
                if let Some(v) = map.get_mut(combiner) {
                    sanitize_json_schema(v);
                }
            }

            // Normalize/ensure type
            let mut ty = map.get("type").and_then(|v| v.as_str()).map(str::to_string);

            // If type is an array (union), pick first supported; else leave to inference
            if ty.is_none()
                && let Some(JsonValue::Array(types)) = map.get("type")
            {
                for t in types {
                    if let Some(tt) = t.as_str()
                        && matches!(
                            tt,
                            "object" | "array" | "string" | "number" | "integer" | "boolean"
                        )
                    {
                        ty = Some(tt.to_string());
                        break;
                    }
                }
            }

            // Infer type if still missing
            if ty.is_none() {
                if map.contains_key("properties")
                    || map.contains_key("required")
                    || map.contains_key("additionalProperties")
                {
                    ty = Some("object".to_string());
                } else if map.contains_key("items") || map.contains_key("prefixItems") {
                    ty = Some("array".to_string());
                } else if map.contains_key("enum")
                    || map.contains_key("const")
                    || map.contains_key("format")
                {
                    ty = Some("string".to_string());
                } else if map.contains_key("minimum")
                    || map.contains_key("maximum")
                    || map.contains_key("exclusiveMinimum")
                    || map.contains_key("exclusiveMaximum")
                    || map.contains_key("multipleOf")
                {
                    ty = Some("number".to_string());
                }
            }
            // If we still couldn't infer, default to string
            let ty = ty.unwrap_or_else(|| "string".to_string());
            map.insert("type".to_string(), JsonValue::String(ty.to_string()));

            // Ensure object schemas have properties map
            if ty == "object" {
                if !map.contains_key("properties") {
                    map.insert(
                        "properties".to_string(),
                        JsonValue::Object(serde_json::Map::new()),
                    );
                }
                // If additionalProperties is an object schema, sanitize it too.
                // Leave booleans as-is, since JSON Schema allows boolean here.
                if let Some(ap) = map.get_mut("additionalProperties") {
                    let is_bool = matches!(ap, JsonValue::Bool(_));
                    if !is_bool {
                        sanitize_json_schema(ap);
                    }
                }
            }

            // Ensure array schemas have items
            if ty == "array" && !map.contains_key("items") {
                map.insert("items".to_string(), json!({ "type": "string" }));
            }
        }
        _ => {}
    }
}
