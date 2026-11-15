use chaos_abi::AbiError;
use chaos_abi::ContentItem;
use chaos_abi::ResponseItem;
use chaos_abi::ToolDef;
use chaos_abi::TurnRequest;
use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::models::LocalShellAction;
use chaos_ipc::models::ReasoningItemContent;
use chaos_ipc::models::ReasoningItemReasoningSummary;
use chaos_ipc::openai_models::ReasoningEffort;
use lsd_protocol::ChatCompletionParams;
use lsd_protocol::Content;
use lsd_protocol::Effort;
use lsd_protocol::File;
use lsd_protocol::InferenceRequest;
use lsd_protocol::Input;
use lsd_protocol::Message;
use lsd_protocol::Params;
use lsd_protocol::Role;
use lsd_protocol::System;
use lsd_protocol::ThinkingDisplay;
use lsd_protocol::Thought;
use lsd_protocol::Tool;
use lsd_protocol::ToolCall;
use lsd_protocol::ToolResult;
use rama::telemetry::tracing;
use serde_json::Value;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

/// A target holding `::` names a model (`anthropic::claude-opus-5-5`); a bare
/// name is a function configured on the server.
pub(super) fn target(request: &mut InferenceRequest, name: &str) {
    if name.contains("::") {
        request.model_name = Some(name.to_string());
    } else {
        request.function_name = Some(name.to_string());
    }
}

/// The full request for this turn: every message, no trip continuation yet.
pub(super) fn build_request(
    request: &TurnRequest,
    target_name: &str,
) -> Result<InferenceRequest, AbiError> {
    let max_tokens = crate::common::max_output_tokens(request)?.or_else(|| {
        request
            .extensions
            .get("max_tokens")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    });
    let temperature = request
        .extensions
        .get("temperature")
        .and_then(Value::as_f64)
        .map(|value| value as f32);
    let reasoning = request.reasoning.as_ref();
    let chat_completion = ChatCompletionParams {
        max_tokens,
        temperature,
        reasoning_effort: reasoning.and_then(|r| r.effort).and_then(effort),
        thinking_display: reasoning
            .and_then(|r| r.summary)
            .map(|summary| match summary {
                ReasoningSummary::None => ThinkingDisplay::Omitted,
                ReasoningSummary::Auto | ReasoningSummary::Concise | ReasoningSummary::Detailed => {
                    ThinkingDisplay::Summarized
                }
            }),
        ..Default::default()
    };

    let mut wire = InferenceRequest {
        input: Input {
            system: (!request.instructions.is_empty())
                .then(|| System::Text(request.instructions.clone())),
            messages: messages(&request.input),
        },
        stream: Some(true),
        params: Params { chat_completion },
        additional_tools: (!request.tools.is_empty())
            .then(|| request.tools.iter().map(tool).collect()),
        parallel_tool_calls: (!request.tools.is_empty()).then_some(request.parallel_tool_calls),
        ..Default::default()
    };
    target(&mut wire, target_name);
    Ok(wire)
}

/// Pins what the server stored for a trip: the target, system prompt, tools
/// and the first `prefix_len` messages. Any change means the trip no longer
/// matches this conversation and the whole history must be sent again.
pub(super) fn fingerprint(request: &InferenceRequest, prefix_len: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    request.function_name.hash(&mut hasher);
    request.model_name.hash(&mut hasher);
    for part in [
        serde_json::to_string(&request.input.system),
        serde_json::to_string(&request.additional_tools),
        serde_json::to_string(
            &request.input.messages[..prefix_len.min(request.input.messages.len())],
        ),
    ] {
        part.unwrap_or_default().hash(&mut hasher);
    }
    hasher.finish()
}

fn effort(effort: ReasoningEffort) -> Option<Effort> {
    Some(match effort {
        ReasoningEffort::None => Effort::None,
        ReasoningEffort::Minimal => Effort::Minimal,
        ReasoningEffort::Low => Effort::Low,
        ReasoningEffort::Medium => Effort::Medium,
        ReasoningEffort::High => Effort::High,
        ReasoningEffort::XHigh => Effort::Xhigh,
        ReasoningEffort::Max | ReasoningEffort::Ultra => Effort::Max,
    })
}

fn tool(tool: &ToolDef) -> Tool {
    match tool {
        ToolDef::Function(f) => Tool::Function {
            name: f.name.clone(),
            description: f.description.clone(),
            parameters: f.parameters.clone(),
            strict: f.strict,
        },
        ToolDef::Freeform(f) => Tool::Function {
            name: f.name.clone(),
            description: format!("{}\n\nFormat: {}", f.description, f.definition),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": format!("Input in {} format: {}", f.format_type, f.syntax),
                    }
                },
                "required": ["input"],
            }),
            strict: false,
        },
    }
}

/// Consecutive items of one role share a message, so a reply's reasoning,
/// text and tool calls travel together (providers require a signed thought to
/// sit beside the tool calls it led to) and parallel tool results arrive as
/// one turn.
pub(super) fn messages(input: &[ResponseItem]) -> Vec<Message> {
    let mut messages: Vec<Message> = Vec::new();
    for item in input {
        let Some((role, content)) = item_content(item) else {
            continue;
        };
        match messages.last_mut() {
            Some(last) if last.role == role && role != Role::System => last.content.extend(content),
            _ => messages.push(Message { role, content }),
        }
    }
    messages
}

fn item_content(item: &ResponseItem) -> Option<(Role, Vec<Content>)> {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let content: Vec<Content> = content.iter().map(content_item).collect();
            let role = match role.as_str() {
                "assistant" => Role::Assistant,
                "system" | "developer" => Role::System,
                _ => Role::User,
            };
            (!content.is_empty()).then_some((role, content))
        }
        ResponseItem::Reasoning {
            summary,
            content,
            encrypted_content,
            ..
        } => {
            let text = content
                .iter()
                .flatten()
                .map(|part| match part {
                    ReasoningItemContent::ReasoningText { text }
                    | ReasoningItemContent::Text { text } => text.as_str(),
                })
                .collect::<String>();
            let summary: Vec<lsd_protocol::content::ThoughtSummary> = summary
                .iter()
                .map(|ReasoningItemReasoningSummary::SummaryText { text }| {
                    lsd_protocol::content::ThoughtSummary::SummaryText { text: text.clone() }
                })
                .collect();
            if text.is_empty() && summary.is_empty() && encrypted_content.is_none() {
                return None;
            }
            Some((
                Role::Assistant,
                vec![Content::Thought(Thought {
                    text: Some(text),
                    signature: encrypted_content.clone(),
                    summary: (!summary.is_empty()).then_some(summary),
                    ..Default::default()
                })],
            ))
        }
        ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } => Some((
            Role::Assistant,
            vec![Content::ToolCall(ToolCall {
                id: call_id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            })],
        )),
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            ..
        } => Some((
            Role::Assistant,
            vec![Content::ToolCall(ToolCall {
                id: call_id.clone(),
                name: name.clone(),
                arguments: serde_json::json!({ "input": input }).to_string(),
            })],
        )),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            action: LocalShellAction::Exec(exec),
            ..
        } => Some((
            Role::Assistant,
            vec![Content::ToolCall(ToolCall {
                id: call_id.clone(),
                name: "shell".to_string(),
                arguments: serde_json::json!({ "command": exec.command.join(" ") }).to_string(),
            })],
        )),
        ResponseItem::FunctionCallOutput {
            call_id,
            output,
            tool_name,
        }
        | ResponseItem::CustomToolCallOutput {
            call_id,
            output,
            tool_name,
        } => Some((
            Role::User,
            vec![Content::ToolResult(ToolResult {
                id: call_id.clone(),
                name: tool_name.clone().unwrap_or_else(|| "tool".to_string()),
                result: crate::common::function_output_text(&output.body),
                is_error: output.success == Some(false),
            })],
        )),
        _ => {
            tracing::debug!("LSD adapter: skipping a history item the native API has no shape for");
            None
        }
    }
}

fn content_item(item: &ContentItem) -> Content {
    match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
            Content::text(text.clone())
        }
        ContentItem::InputImage { image_url } => Content::File(image(image_url)),
        ContentItem::Document { name, text, .. } => {
            let header = name
                .as_deref()
                .map(|n| format!("[{n}]\n"))
                .unwrap_or_default();
            Content::text(format!("{header}{text}"))
        }
    }
}

fn image(url: &str) -> File {
    if let Some((meta, data)) = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        && let Some(mime_type) = meta.strip_suffix(";base64")
    {
        return File::Base64 {
            mime_type: mime_type.to_string(),
            data: data.to_string(),
            source_url: None,
            detail: None,
            filename: None,
        };
    }
    File::Url {
        url: url.to_string(),
        mime_type: None,
        detail: None,
        filename: None,
    }
}
