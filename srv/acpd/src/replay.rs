use std::collections::HashSet;

use acp_host::{SessionContext, protocol};
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::{
    ContentItem, FunctionCallOutputBody, FunctionCallOutputContentItem, FunctionCallOutputPayload,
    ResponseItem,
};
use chaos_ipc::protocol::{EventMsg, RolloutItem};
use protocol::schema::v1::{
    ContentBlock, ContentChunk, SessionUpdate, TextContent, ToolCall, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};

use crate::projection::{preview, tool_preview};

async fn text(context: &SessionContext, mut text: &str, user: bool) -> protocol::Result<()> {
    while !text.is_empty() {
        let mut end = text.len().min(8 * 1024);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(&text[..end])));
        context
            .client()
            .update_wait(if user {
                SessionUpdate::UserMessageChunk(chunk)
            } else {
                SessionUpdate::AgentMessageChunk(chunk)
            })
            .await?;
        text = &text[end..];
    }
    Ok(())
}

fn output_preview(output: &FunctionCallOutputPayload) -> String {
    match &output.body {
        FunctionCallOutputBody::Text(value) => {
            if let Ok(mcp) = serde_json::from_str::<chaos_ipc::mcp::CallToolResult>(value) {
                return tool_preview(&Ok(mcp));
            }
            preview(value)
        }
        FunctionCallOutputBody::ContentItems(items) => {
            let mut text = String::new();
            for item in items {
                if let FunctionCallOutputContentItem::InputText { text: value } = item {
                    text.push_str(&preview(value));
                    text.push('\n');
                    if text.len() >= 16 * 1024 {
                        break;
                    }
                }
            }
            if text.is_empty() {
                "[Non-text tool output omitted]".into()
            } else {
                preview(&text)
            }
        }
    }
}

async fn tool(context: &SessionContext, id: &str, name: &str) -> protocol::Result<()> {
    let kind = match name {
        "read_file" => ToolKind::Read,
        "apply_patch" => ToolKind::Edit,
        "exec_command" => ToolKind::Execute,
        _ => ToolKind::Other,
    };
    context
        .client()
        .update_wait(SessionUpdate::ToolCall(
            ToolCall::new(id.to_string(), name.chars().take(256).collect::<String>())
                .kind(kind)
                .status(ToolCallStatus::InProgress),
        ))
        .await
}

async fn result(
    context: &SessionContext,
    id: &str,
    success: bool,
    text: String,
) -> protocol::Result<()> {
    context
        .client()
        .update_wait(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            id.to_string(),
            ToolCallUpdateFields::new()
                .status(if success {
                    ToolCallStatus::Completed
                } else {
                    ToolCallStatus::Failed
                })
                .content(vec![ContentBlock::Text(TextContent::new(text)).into()]),
        )))
        .await
}

/// Project only explicit public user items, assistant text and tool previews.
/// Native contexts, reasoning, provider metadata and legacy duplicate events stay private.
pub(crate) async fn history(
    items: Vec<RolloutItem>,
    context: &SessionContext,
) -> protocol::Result<()> {
    let mut pending = HashSet::new();
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    for item in items {
        match item {
            RolloutItem::EventMsg(EventMsg::ItemCompleted(event)) => {
                if let TurnItem::UserMessage(user) = event.item {
                    text(context, &user.message(), true).await?;
                }
            }
            RolloutItem::ResponseItem(ResponseItem::Message { role, content, .. })
                if role == "assistant" =>
            {
                for content in content {
                    if let ContentItem::InputText { text: value }
                    | ContentItem::OutputText { text: value } = content
                    {
                        text(context, &value, false).await?;
                    }
                }
            }
            RolloutItem::ResponseItem(
                ResponseItem::FunctionCall { call_id, name, .. }
                | ResponseItem::CustomToolCall { call_id, name, .. },
            ) => {
                if seen.insert(call_id.clone()) {
                    tool(context, &call_id, &name).await?;
                    order.push(call_id.clone());
                    pending.insert(call_id);
                }
            }
            RolloutItem::ResponseItem(
                ResponseItem::FunctionCallOutput {
                    call_id, output, ..
                }
                | ResponseItem::CustomToolCallOutput {
                    call_id, output, ..
                },
            ) if pending.remove(&call_id) => {
                result(
                    context,
                    &call_id,
                    output.success != Some(false),
                    output_preview(&output),
                )
                .await?;
            }
            _ => {}
        }
    }
    for id in order.into_iter().filter(|id| pending.contains(id)) {
        result(
            context,
            &id,
            false,
            "Previous tool outcome is unknown; work was not replayed.".into(),
        )
        .await?;
    }
    Ok(())
}
