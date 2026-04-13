use std::collections::HashSet;

use chaos_ipc::api::McpElicitationEnumSchema;
use chaos_ipc::api::McpElicitationPrimitiveSchema;
use chaos_ipc::api::McpElicitationSingleSelectEnumSchema;
use serde_json::Value;

use super::domain::{
    APPROVAL_META_KIND_KEY, APPROVAL_META_KIND_TOOL_SUGGESTION, APPROVAL_PERSIST_KEY,
    APPROVAL_TOOL_PARAMS_DISPLAY_KEY, APPROVAL_TOOL_PARAMS_KEY, TOOL_ID_KEY, TOOL_NAME_KEY,
    TOOL_SUGGEST_INSTALL_URL_KEY, TOOL_SUGGEST_REASON_KEY, TOOL_SUGGEST_SUGGEST_TYPE_KEY,
    TOOL_TYPE_KEY,
};
use super::domain::{
    McpServerElicitationField, McpServerElicitationFieldInput, McpServerElicitationOption,
    McpToolApprovalDisplayParam, ToolSuggestionRequest, ToolSuggestionToolType, ToolSuggestionType,
};

pub(super) fn parse_tool_suggestion_request(meta: Option<&Value>) -> Option<ToolSuggestionRequest> {
    let meta = meta?.as_object()?;
    if meta.get(APPROVAL_META_KIND_KEY).and_then(Value::as_str)
        != Some(APPROVAL_META_KIND_TOOL_SUGGESTION)
    {
        return None;
    }

    let tool_type = match meta.get(TOOL_TYPE_KEY).and_then(Value::as_str) {
        Some("connector") => ToolSuggestionToolType::Connector,
        _ => return None,
    };
    let suggest_type = match meta
        .get(TOOL_SUGGEST_SUGGEST_TYPE_KEY)
        .and_then(Value::as_str)
    {
        Some("install") => ToolSuggestionType::Install,
        Some("enable") => ToolSuggestionType::Enable,
        _ => return None,
    };

    Some(ToolSuggestionRequest {
        tool_type,
        suggest_type,
        suggest_reason: meta
            .get(TOOL_SUGGEST_REASON_KEY)
            .and_then(Value::as_str)?
            .to_string(),
        tool_id: meta.get(TOOL_ID_KEY).and_then(Value::as_str)?.to_string(),
        tool_name: meta.get(TOOL_NAME_KEY).and_then(Value::as_str)?.to_string(),
        install_url: meta
            .get(TOOL_SUGGEST_INSTALL_URL_KEY)
            .and_then(Value::as_str)?
            .to_string(),
    })
}

pub(super) fn tool_approval_supports_persist_mode(
    meta: Option<&Value>,
    expected_mode: &str,
) -> bool {
    let Some(persist) = meta
        .and_then(Value::as_object)
        .and_then(|meta| meta.get(APPROVAL_PERSIST_KEY))
    else {
        return false;
    };

    match persist {
        Value::String(value) => value == expected_mode,
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .any(|value| value == expected_mode),
        _ => false,
    }
}

pub(super) fn parse_tool_approval_display_params(
    meta: Option<&Value>,
) -> Vec<McpToolApprovalDisplayParam> {
    let Some(meta) = meta.and_then(Value::as_object) else {
        return Vec::new();
    };

    let display_params = meta
        .get(APPROVAL_TOOL_PARAMS_DISPLAY_KEY)
        .and_then(Value::as_array)
        .map(|display_params| {
            display_params
                .iter()
                .filter_map(parse_tool_approval_display_param)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !display_params.is_empty() {
        return display_params;
    }

    let mut fallback_params = meta
        .get(APPROVAL_TOOL_PARAMS_KEY)
        .and_then(Value::as_object)
        .map(|tool_params| {
            tool_params
                .iter()
                .map(|(name, value)| McpToolApprovalDisplayParam {
                    name: name.clone(),
                    value: value.clone(),
                    display_name: name.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    fallback_params.sort_by(|left, right| left.name.cmp(&right.name));
    fallback_params
}

pub(super) fn parse_tool_approval_display_param(
    value: &Value,
) -> Option<McpToolApprovalDisplayParam> {
    let value = value.as_object()?;
    let name = value.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let display_name = value
        .get("display_name")
        .and_then(Value::as_str)
        .unwrap_or(name)
        .trim();
    if display_name.is_empty() {
        return None;
    }
    Some(McpToolApprovalDisplayParam {
        name: name.to_string(),
        value: value.get("value")?.clone(),
        display_name: display_name.to_string(),
    })
}

pub(super) fn parse_fields_from_schema(
    requested_schema: &Value,
) -> Option<Vec<McpServerElicitationField>> {
    let schema = requested_schema.as_object()?;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return None;
    }
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<HashSet<_>>();
    let properties = schema.get("properties")?.as_object()?;
    let mut fields = Vec::new();
    for (id, property_schema) in properties {
        let property =
            serde_json::from_value::<McpElicitationPrimitiveSchema>(property_schema.clone())
                .ok()?;
        fields.push(parse_field(id, property, required.contains(id))?);
    }
    if fields.is_empty() {
        return None;
    }
    Some(fields)
}

pub(super) fn parse_field(
    id: &str,
    property: McpElicitationPrimitiveSchema,
    required: bool,
) -> Option<McpServerElicitationField> {
    match property {
        McpElicitationPrimitiveSchema::String(schema) => {
            let label = schema.title.unwrap_or_else(|| id.to_string());
            let prompt = schema.description.unwrap_or_else(|| label.clone());
            Some(McpServerElicitationField {
                id: id.to_string(),
                label,
                prompt,
                required,
                input: McpServerElicitationFieldInput::Text { secret: false },
            })
        }
        McpElicitationPrimitiveSchema::Boolean(schema) => {
            let label = schema.title.unwrap_or_else(|| id.to_string());
            let prompt = schema.description.unwrap_or_else(|| label.clone());
            let default_idx = schema.default.map(|value| if value { 0 } else { 1 });
            let options = [true, false]
                .into_iter()
                .map(|value| {
                    let label = if value { "True" } else { "False" }.to_string();
                    McpServerElicitationOption {
                        label,
                        description: None,
                        value: Value::Bool(value),
                    }
                })
                .collect();
            Some(McpServerElicitationField {
                id: id.to_string(),
                label,
                prompt,
                required,
                input: McpServerElicitationFieldInput::Select {
                    options,
                    default_idx,
                },
            })
        }
        McpElicitationPrimitiveSchema::Enum(McpElicitationEnumSchema::Legacy(schema)) => {
            let label = schema.title.unwrap_or_else(|| id.to_string());
            let prompt = schema.description.unwrap_or_else(|| label.clone());
            let default_idx = schema
                .default
                .as_ref()
                .and_then(|value| schema.enum_.iter().position(|entry| entry == value));
            let enum_names = schema.enum_names.unwrap_or_default();
            let options = schema
                .enum_
                .into_iter()
                .enumerate()
                .map(|(idx, value)| McpServerElicitationOption {
                    label: enum_names
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| value.clone()),
                    description: None,
                    value: Value::String(value),
                })
                .collect();
            Some(McpServerElicitationField {
                id: id.to_string(),
                label,
                prompt,
                required,
                input: McpServerElicitationFieldInput::Select {
                    options,
                    default_idx,
                },
            })
        }
        McpElicitationPrimitiveSchema::Enum(McpElicitationEnumSchema::SingleSelect(schema)) => {
            parse_single_select_field(id, schema, required)
        }
        McpElicitationPrimitiveSchema::Number(_)
        | McpElicitationPrimitiveSchema::Enum(McpElicitationEnumSchema::MultiSelect(_)) => None,
    }
}

pub(super) fn parse_single_select_field(
    id: &str,
    schema: McpElicitationSingleSelectEnumSchema,
    required: bool,
) -> Option<McpServerElicitationField> {
    match schema {
        McpElicitationSingleSelectEnumSchema::Untitled(schema) => {
            let label = schema.title.unwrap_or_else(|| id.to_string());
            let prompt = schema.description.unwrap_or_else(|| label.clone());
            let default_idx = schema
                .default
                .as_ref()
                .and_then(|value| schema.enum_.iter().position(|entry| entry == value));
            let options = schema
                .enum_
                .into_iter()
                .map(|value| McpServerElicitationOption {
                    label: value.clone(),
                    description: None,
                    value: Value::String(value),
                })
                .collect();
            Some(McpServerElicitationField {
                id: id.to_string(),
                label,
                prompt,
                required,
                input: McpServerElicitationFieldInput::Select {
                    options,
                    default_idx,
                },
            })
        }
        McpElicitationSingleSelectEnumSchema::Titled(schema) => {
            let label = schema.title.unwrap_or_else(|| id.to_string());
            let prompt = schema.description.unwrap_or_else(|| label.clone());
            let default_idx = schema.default.as_ref().and_then(|value| {
                schema
                    .one_of
                    .iter()
                    .position(|entry| entry.const_.as_str() == value)
            });
            let options = schema
                .one_of
                .into_iter()
                .map(|entry| McpServerElicitationOption {
                    label: entry.title,
                    description: None,
                    value: Value::String(entry.const_),
                })
                .collect();
            Some(McpServerElicitationField {
                id: id.to_string(),
                label,
                prompt,
                required,
                input: McpServerElicitationFieldInput::Select {
                    options,
                    default_idx,
                },
            })
        }
    }
}
