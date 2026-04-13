use serde_json::Value;

use crate::text_formatting::format_json_compact;
use crate::text_formatting::truncate_text;

use super::domain::{
    APPROVAL_TOOL_PARAM_DISPLAY_LIMIT, APPROVAL_TOOL_PARAM_VALUE_TRUNCATE_GRAPHEMES,
    McpToolApprovalDisplayParam,
};

pub(super) fn format_tool_approval_display_message(
    message: &str,
    approval_display_params: &[McpToolApprovalDisplayParam],
) -> String {
    let message = message.trim();
    if approval_display_params.is_empty() {
        return message.to_string();
    }

    let mut sections = Vec::new();
    if !message.is_empty() {
        sections.push(message.to_string());
    }
    let param_lines = approval_display_params
        .iter()
        .take(APPROVAL_TOOL_PARAM_DISPLAY_LIMIT)
        .map(format_tool_approval_display_param_line)
        .collect::<Vec<_>>();
    if !param_lines.is_empty() {
        sections.push(param_lines.join("\n"));
    }
    let mut message = sections.join("\n\n");
    message.push('\n');
    message
}

pub(super) fn format_tool_approval_display_param_line(
    param: &McpToolApprovalDisplayParam,
) -> String {
    format!(
        "{}: {}",
        param.display_name,
        format_tool_approval_display_param_value(&param.value)
    )
}

pub(super) fn format_tool_approval_display_param_value(value: &Value) -> String {
    let formatted = match value {
        Value::String(text) => text.split_whitespace().collect::<Vec<_>>().join(" "),
        _ => {
            let compact_json = value.to_string();
            format_json_compact(&compact_json).unwrap_or(compact_json)
        }
    };
    truncate_text(&formatted, APPROVAL_TOOL_PARAM_VALUE_TRUNCATE_GRAPHEMES)
}
