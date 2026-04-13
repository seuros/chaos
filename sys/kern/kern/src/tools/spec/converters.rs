use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_parrot::sanitize::ResponsesApiTool;
use chaos_parrot::sanitize::mcp_tool_to_responses_api_tool;
use chaos_parrot::sanitize::parse_tool_input_schema;
use serde::Deserialize;
use serde::Serialize;

use crate::client_common::tools::ToolSpec;

/// TODO(dylan): deprecate once we get rid of json tool
#[derive(Serialize, Deserialize)]
pub(crate) struct ApplyPatchToolArgs {
    pub(crate) input: String,
}

/// Returns JSON values that are compatible with Function Calling in the
/// Responses API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=responses
pub fn create_tools_json_for_responses_api(
    tools: &[ToolSpec],
) -> crate::error::Result<Vec<serde_json::Value>> {
    let mut tools_json = Vec::new();

    for tool in tools {
        let json = serde_json::to_value(tool)?;
        tools_json.push(json);
    }

    Ok(tools_json)
}

/// Build a compact bracketed suffix from MCP tool annotations.
///
/// Only includes hints for fields that are explicitly set:
/// - `read_only_hint: Some(true)` -> `"read-only"`
/// - `read_only_hint: Some(false)` -> `"writes"`
/// - `destructive_hint: Some(true)` -> `"destructive"`
/// - `idempotent_hint: Some(true)` -> `"idempotent"`
/// - `open_world_hint: Some(true)` -> `"open-world"`
/// - `open_world_hint: Some(false)` -> `"closed-world"`
pub(crate) fn annotation_suffix(annotations: &chaos_mcp_runtime::ToolAnnotations) -> String {
    let hints = annotation_labels(annotations);

    if hints.is_empty() {
        String::new()
    } else {
        format!(" [{}]", hints.join(", "))
    }
}

pub(crate) fn annotation_labels(annotations: &chaos_mcp_runtime::ToolAnnotations) -> Vec<String> {
    let mut hints: Vec<String> = Vec::new();

    match annotations.read_only_hint {
        Some(true) => hints.push("read-only".to_string()),
        Some(false) => hints.push("writes".to_string()),
        None => {}
    }
    if annotations.destructive_hint == Some(true) {
        hints.push("destructive".to_string());
    }
    if annotations.idempotent_hint == Some(true) {
        hints.push("idempotent".to_string());
    }
    match annotations.open_world_hint {
        Some(true) => hints.push("open-world".to_string()),
        Some(false) => hints.push("closed-world".to_string()),
        None => {}
    }

    hints
}

pub(crate) fn mcp_tool_to_openai_tool(
    fully_qualified_name: String,
    tool: chaos_mcp_runtime::manager::McpToolInfo,
) -> Result<ResponsesApiTool, serde_json::Error> {
    let description = match (&tool.description, &tool.annotations) {
        (Some(desc), Some(ann)) => {
            let suffix = annotation_suffix(ann);
            if suffix.is_empty() {
                Some(desc.clone())
            } else {
                Some(format!("{desc}{suffix}"))
            }
        }
        (desc, _) => desc.clone(),
    };
    mcp_tool_to_responses_api_tool(
        fully_qualified_name,
        description,
        tool.input_schema,
        tool.output_schema,
        false,
    )
}

#[cfg(test)]
pub(crate) fn mcp_tool_to_deferred_openai_tool(
    name: String,
    tool: chaos_mcp_runtime::manager::McpToolInfo,
) -> Result<ResponsesApiTool, serde_json::Error> {
    let description = match (&tool.description, &tool.annotations) {
        (Some(desc), Some(ann)) => {
            let suffix = annotation_suffix(ann);
            if suffix.is_empty() {
                Some(desc.clone())
            } else {
                Some(format!("{desc}{suffix}"))
            }
        }
        (desc, _) => desc.clone(),
    };
    mcp_tool_to_responses_api_tool(name, description, tool.input_schema, None, true)
}

pub(crate) fn dynamic_tool_to_openai_tool(
    tool: &DynamicToolSpec,
) -> Result<ResponsesApiTool, serde_json::Error> {
    let input_schema = parse_tool_input_schema(&tool.input_schema)?;

    Ok(ResponsesApiTool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        strict: false,
        defer_loading: None,
        parameters: input_schema,
        output_schema: None,
    })
}
