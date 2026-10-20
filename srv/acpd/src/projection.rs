use std::collections::HashMap;

use acp_host::{SessionContext, protocol};
use chaos_ipc::protocol::EventMsg;
use protocol::schema::v1::{
    ContentBlock, ContentChunk, SessionUpdate, TextContent, ToolCall, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind, UsageUpdate,
};
use serde_json::Value;

use crate::internal;

/// Only public assistant text and tool events cross the editor boundary.
#[derive(Default)]
pub(crate) struct Projection {
    tools: HashMap<String, ToolCallStatus>,
    streamed_message: bool,
    any_text: bool,
}

impl Projection {
    fn text(&mut self, context: &SessionContext, text: &str) -> protocol::Result<()> {
        if !text.is_empty() {
            context
                .client()
                .update(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text)),
                )))?;
            self.any_text = true;
        }
        Ok(())
    }

    pub fn finish_text(
        &mut self,
        context: &SessionContext,
        fallback: Option<&str>,
    ) -> protocol::Result<()> {
        if !self.any_text {
            self.text(context, fallback.unwrap_or_default())?;
        }
        Ok(())
    }

    pub fn cancelled(&mut self, context: &SessionContext) -> protocol::Result<()> {
        let unfinished: Vec<_> = self
            .tools
            .iter()
            .filter(|(_, status)| {
                matches!(status, ToolCallStatus::Pending | ToolCallStatus::InProgress)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in unfinished {
            self.finish_tool(context, &id, false, "Cancelled".into())?;
        }
        Ok(())
    }

    pub fn tool(
        &mut self,
        context: &SessionContext,
        id: &str,
        title: String,
        kind: ToolKind,
        status: ToolCallStatus,
        input: Option<Value>,
    ) -> protocol::Result<ToolCallUpdate> {
        let fields = ToolCallUpdateFields::new()
            .title(title)
            .kind(kind)
            .status(status)
            .raw_input(input);
        let update = ToolCallUpdate::new(id.to_owned(), fields);
        if self.tools.contains_key(id) {
            context
                .client()
                .update(SessionUpdate::ToolCallUpdate(update.clone()))?;
        } else {
            if self.tools.len() >= 1024 {
                return Err(internal("ACP native turn tool limit exceeded"));
            }
            context
                .client()
                .update(SessionUpdate::ToolCall(ToolCall::try_from(update.clone())?))?;
        }
        self.tools.insert(id.to_owned(), status);
        Ok(update)
    }

    fn finish_tool(
        &mut self,
        context: &SessionContext,
        id: &str,
        success: bool,
        output: String,
    ) -> protocol::Result<()> {
        if matches!(
            self.tools.get(id),
            Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
        ) {
            return Ok(());
        }
        if !self.tools.contains_key(id) {
            self.tool(
                context,
                id,
                "Tool result".into(),
                ToolKind::Other,
                ToolCallStatus::InProgress,
                None,
            )?;
        }
        let status = if success {
            ToolCallStatus::Completed
        } else {
            ToolCallStatus::Failed
        };
        // Tool results can dwarf the wire frame. This is an editor preview, not
        // a second transcript or an unbounded stdout accumulator.
        let output = preview(&output);
        context
            .client()
            .update(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                id.to_owned(),
                ToolCallUpdateFields::new()
                    .status(status)
                    .raw_output(serde_json::json!({"preview":output}))
                    .content(vec![ContentBlock::Text(TextContent::new(output)).into()]),
            )))?;
        self.tools.insert(id.to_owned(), status);
        Ok(())
    }

    pub fn event(&mut self, context: &SessionContext, event: &EventMsg) -> protocol::Result<()> {
        match event {
            EventMsg::AgentMessageContentDelta(event) => {
                self.text(context, &event.delta)?;
                self.streamed_message = true;
            }
            EventMsg::AgentMessage(event) => {
                if !self.streamed_message {
                    self.text(context, &event.message)?;
                }
                self.streamed_message = false;
            }
            EventMsg::McpToolCallBegin(event) => {
                self.tool(
                    context,
                    &event.call_id,
                    event.invocation.display_name(),
                    ToolKind::Other,
                    ToolCallStatus::InProgress,
                    event.invocation.arguments.clone(),
                )?;
            }
            EventMsg::McpToolCallEnd(event) => {
                let output = tool_preview(&event.result);
                self.finish_tool(context, &event.call_id, event.is_success(), output)?;
            }
            EventMsg::ExecCommandBegin(event) => {
                self.tool(
                    context,
                    &event.call_id,
                    event.command.join(" "),
                    ToolKind::Execute,
                    ToolCallStatus::InProgress,
                    Some(serde_json::json!({"command":event.command,"cwd":event.cwd})),
                )?;
            }
            EventMsg::ExecCommandEnd(event) => {
                self.finish_tool(
                    context,
                    &event.call_id,
                    matches!(
                        event.status,
                        chaos_ipc::protocol::ExecCommandStatus::Completed
                    ),
                    event.formatted_output.clone(),
                )?;
            }
            EventMsg::PatchApplyBegin(event) => {
                self.tool(
                    context,
                    &event.call_id,
                    "Apply patch".into(),
                    ToolKind::Edit,
                    ToolCallStatus::InProgress,
                    Some(serde_json::json!({"changes":event.changes})),
                )?;
            }
            EventMsg::PatchApplyEnd(event) => {
                self.finish_tool(
                    context,
                    &event.call_id,
                    event.success,
                    format!("{}{}", event.stdout, event.stderr),
                )?;
            }
            EventMsg::TokenCount(event) => {
                if let Some(info) = &event.info
                    && let Some(window) = info.model_context_window
                    && window > 0
                    && info.last_token_usage.total_tokens >= 0
                {
                    context
                        .client()
                        .update(SessionUpdate::UsageUpdate(UsageUpdate::new(
                            info.last_token_usage.total_tokens as u64,
                            window as u64,
                        )))?;
                }
            }
            // Reasoning, raw provider responses and internal/system messages
            // are deliberately excluded, even when native settings enable them.
            _ => {}
        }
        Ok(())
    }
}

pub(crate) fn tool_preview(result: &Result<chaos_ipc::mcp::CallToolResult, String>) -> String {
    let result = match result {
        Ok(result) => result,
        Err(message) => return preview(message),
    };
    let mut output = String::new();
    for content in &result.content {
        if content["type"] == "text"
            && let Some(text) = content["text"].as_str()
        {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&preview(text));
            if output.len() >= 16 * 1024 {
                break;
            }
        }
    }
    // Never serialize arbitrary MCP envelopes: _meta can be host-only, and
    // binary/resource payloads are not baseline ACP text previews.
    if output.is_empty() {
        "[Non-text tool output omitted]".into()
    } else {
        preview(&output)
    }
}

pub(crate) fn preview(value: &str) -> String {
    let mut end = value.len().min(16 * 1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    if end == value.len() {
        value.to_owned()
    } else {
        format!("{}\n[preview truncated]", &value[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_preview_excludes_private_metadata_and_binary_content() {
        let result = chaos_ipc::mcp::CallToolResult {
            content: vec![
                serde_json::json!({"type":"text","text":"public tool result"}),
                serde_json::json!({"type":"image","data":"private-binary"}),
            ],
            structured_content: None,
            is_error: None,
            meta: Some(serde_json::json!({"private":"host-only metadata"})),
        };
        assert_eq!(tool_preview(&Ok(result)), "public tool result");
    }

    #[test]
    fn tool_previews_are_utf8_safe_and_bounded() {
        let output = preview(&"🦀".repeat(20_000));
        assert!(output.len() <= 16 * 1024 + 32);
        assert!(output.ends_with("[preview truncated]"));
    }
}
