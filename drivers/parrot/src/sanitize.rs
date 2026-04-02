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

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;
use serde_json::{self};

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

/// Converts raw MCP tool fields into an OpenAI [`ResponsesApiTool`].
///
/// Takes the provider-neutral fields (description, input_schema, output_schema)
/// and produces the OpenAI-specific tool definition with sanitized schemas.
/// The kernel calls this with fields destructured from `McpToolInfo` so that
/// `chaos-parrot` never depends on MCP types.
pub fn mcp_tool_to_responses_api_tool(
    name: String,
    description: Option<String>,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
    deferred: bool,
) -> Result<ResponsesApiTool, serde_json::Error> {
    let mut schema = input_schema;

    // OpenAI models mandate the "properties" field in the schema. Some MCP
    // servers omit it (or set it to null), so we insert an empty object to
    // match the behavior of the Agents SDK.
    if let JsonValue::Object(obj) = &mut schema
        && obj.get("properties").is_none_or(JsonValue::is_null)
    {
        obj.insert(
            "properties".to_string(),
            JsonValue::Object(serde_json::Map::new()),
        );
    }

    let parameters = parse_tool_input_schema(&schema)?;

    let output_schema = if deferred {
        None
    } else {
        let structured = output_schema.unwrap_or_else(|| JsonValue::Object(serde_json::Map::new()));
        Some(mcp_call_tool_result_output_schema(structured))
    };

    Ok(ResponsesApiTool {
        name,
        description: description.unwrap_or_default(),
        strict: false,
        defer_loading: if deferred { Some(true) } else { None },
        parameters,
        output_schema,
    })
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
/// - Expands local `$ref` pointers and unwraps nullable `anyOf`/`oneOf`
///   wrappers into the underlying non-null schema when possible.
/// - Fills required child fields (e.g. array items, object properties) with
///   permissive defaults when absent.
pub fn sanitize_json_schema(value: &mut JsonValue) {
    let root = value.clone();
    let mut ref_stack = Vec::new();
    expand_local_schema_references(value, &root, &mut ref_stack);
    sanitize_json_schema_subset(value);
}

fn expand_local_schema_references(
    value: &mut JsonValue,
    root: &JsonValue,
    ref_stack: &mut Vec<String>,
) {
    match value {
        JsonValue::Array(arr) => {
            for v in arr.iter_mut() {
                expand_local_schema_references(v, root, ref_stack);
            }
        }
        JsonValue::Object(map) => {
            if let Some(expanded) = resolve_local_ref_object(map, root, ref_stack) {
                *value = expanded;
                expand_local_schema_references(value, root, ref_stack);
                return;
            }

            for child in map.values_mut() {
                expand_local_schema_references(child, root, ref_stack);
            }

            if let Some(collapsed) = collapse_single_schema_combiners(map) {
                *value = collapsed;
                expand_local_schema_references(value, root, ref_stack);
            }
        }
        _ => {}
    }
}

fn resolve_local_ref_object(
    map: &serde_json::Map<String, JsonValue>,
    root: &JsonValue,
    ref_stack: &mut Vec<String>,
) -> Option<JsonValue> {
    let reference = map.get("$ref")?.as_str()?;
    if !reference.starts_with('#') || ref_stack.iter().any(|entry| entry == reference) {
        return None;
    }

    ref_stack.push(reference.to_string());
    let resolved_pointer = if reference == "#" {
        Some(root.clone())
    } else {
        root.pointer(reference.strip_prefix('#')?).cloned()
    };
    let Some(mut resolved) = resolved_pointer else {
        ref_stack.pop();
        return None;
    };
    if let JsonValue::Object(resolved_map) = &mut resolved {
        for (key, value) in map {
            if key != "$ref" {
                resolved_map.insert(key.clone(), value.clone());
            }
        }
    }
    expand_local_schema_references(&mut resolved, root, ref_stack);
    ref_stack.pop();
    Some(resolved)
}

fn collapse_single_schema_combiners(map: &serde_json::Map<String, JsonValue>) -> Option<JsonValue> {
    for combiner in ["anyOf", "oneOf"] {
        if let Some(options) = map.get(combiner).and_then(JsonValue::as_array)
            && let Some(branch) = select_single_non_null_branch(options)
        {
            return Some(merge_schema_branch(map, combiner, branch.clone()));
        }
    }

    if let Some(options) = map.get("allOf").and_then(JsonValue::as_array)
        && let [branch] = options.as_slice()
    {
        return Some(merge_schema_branch(map, "allOf", branch.clone()));
    }

    None
}

fn select_single_non_null_branch(options: &[JsonValue]) -> Option<&JsonValue> {
    let mut branch = None;
    for option in options {
        if is_null_schema(option) {
            continue;
        }
        if branch.is_some() {
            return None;
        }
        branch = Some(option);
    }
    branch
}

fn is_null_schema(value: &JsonValue) -> bool {
    match value {
        JsonValue::Null => true,
        JsonValue::Object(map) => match map.get("type") {
            Some(JsonValue::String(ty)) => ty == "null",
            Some(JsonValue::Array(types)) => {
                !types.is_empty() && types.iter().all(|ty| ty.as_str() == Some("null"))
            }
            _ => map.get("const").is_some_and(JsonValue::is_null),
        },
        _ => false,
    }
}

fn merge_schema_branch(
    original: &serde_json::Map<String, JsonValue>,
    combiner: &str,
    mut branch: JsonValue,
) -> JsonValue {
    if let JsonValue::Object(branch_map) = &mut branch {
        for (key, value) in original {
            if key != combiner {
                branch_map.insert(key.clone(), value.clone());
            }
        }
    }
    branch
}

fn sanitize_json_schema_subset(value: &mut JsonValue) {
    match value {
        JsonValue::Bool(_) => {
            // JSON Schema boolean form: true/false. Coerce to an accept-all string.
            *value = json!({ "type": "string" });
        }
        JsonValue::Array(arr) => {
            for v in arr.iter_mut() {
                sanitize_json_schema_subset(v);
            }
        }
        JsonValue::Object(map) => {
            // First, recursively sanitize known nested schema holders
            if let Some(props) = map.get_mut("properties")
                && let Some(props_map) = props.as_object_mut()
            {
                for (_k, v) in props_map.iter_mut() {
                    sanitize_json_schema_subset(v);
                }
            }
            if let Some(items) = map.get_mut("items") {
                sanitize_json_schema_subset(items);
            }
            // Some schemas use oneOf/anyOf/allOf - sanitize their entries
            for combiner in ["oneOf", "anyOf", "allOf", "prefixItems"] {
                if let Some(v) = map.get_mut(combiner) {
                    sanitize_json_schema_subset(v);
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
                        sanitize_json_schema_subset(ap);
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
