use chaos_abi::AbiError;
use chaos_abi::ContentItem;
use chaos_abi::ResponseItem;
use chaos_abi::TurnRequest;
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub(super) struct ChatRequest {
    model: String,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
}

#[derive(Serialize)]
pub(super) struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
pub(super) struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ChatTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ChatToolFunction,
}

#[derive(Serialize)]
pub(super) struct ChatToolFunction {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    parameters: Value,
}

pub(crate) fn build_request_body(
    request: &TurnRequest,
    model: &str,
    stream: bool,
) -> Result<Value, AbiError> {
    let mut messages: Vec<ChatMessage> = Vec::new();

    // Prepend the system prompt when instructions are non-empty.
    if !request.instructions.is_empty() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(Value::String(request.instructions.clone())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    messages.extend(convert_input_to_messages(&request.input));
    normalize_system_messages(&mut messages);

    let tools = convert_tools(&request.tools);
    let parallel_tool_calls = if request.tools.is_empty() {
        None
    } else {
        Some(request.parallel_tool_calls)
    };

    let body = serde_json::to_value(ChatRequest {
        model: model.to_string(),
        stream,
        messages,
        tools,
        parallel_tool_calls,
        response_format: request.output_schema.as_ref().map(|schema| {
            serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "codex_output_schema",
                    "strict": true,
                    "schema": schema
                }
            })
        }),
        stream_options: stream.then_some(StreamOptions {
            include_usage: true,
        }),
        // Use the modern output+reasoning budget field. An incompatible
        // endpoint must reject this request; never retry without the cap.
        max_completion_tokens: crate::common::max_output_tokens(request)?,
    })
    .map_err(|e| AbiError::InvalidRequest {
        message: e.to_string(),
    })?;

    Ok(body)
}

/// Strict chat templates (Qwen3.6/3.8 under vLLM) reject any `system`
/// message that is not the first message. Merge every system message, in
/// order, into one leading block. Instructions are never relabeled as `user`
/// input; the chat format only carries their cumulative state, not when each
/// one arrived.
pub(super) fn normalize_system_messages(messages: &mut Vec<ChatMessage>) {
    let misplaced = messages
        .iter()
        .skip(1)
        .any(|message| message.role == "system");
    if misplaced {
        let (merged, rest): (Vec<ChatMessage>, Vec<ChatMessage>) = std::mem::take(messages)
            .into_iter()
            .partition(|message| message.role == "system");
        *messages = rest;
        let all_text = merged
            .iter()
            .all(|message| matches!(message.content, Some(Value::String(_))));
        let content = if all_text {
            Value::String(
                merged
                    .iter()
                    .filter_map(|message| message.content.as_ref()?.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            )
        } else {
            let mut parts = Vec::new();
            for message in merged {
                match message.content {
                    Some(Value::String(text)) => {
                        parts.push(serde_json::json!({"type": "text", "text": text}));
                    }
                    Some(Value::Array(items)) => parts.extend(items),
                    _ => {}
                }
            }
            Value::Array(parts)
        };
        messages.insert(
            0,
            ChatMessage {
                role: "system".to_string(),
                content: Some(content),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        );
    }
}

pub(super) fn convert_content_item_to_value(c: &ContentItem) -> Option<Value> {
    match c {
        ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
            Some(serde_json::json!({"type": "text", "text": text}))
        }
        ContentItem::InputImage { image_url } => Some(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": image_url }
        })),
        ContentItem::Document { name, text, .. } => {
            let header = name
                .as_deref()
                .map(|n| format!("[{n}]\n"))
                .unwrap_or_default();
            Some(serde_json::json!({ "type": "text", "text": format!("{header}{text}") }))
        }
    }
}

pub(super) fn convert_input_to_messages(input: &[ResponseItem]) -> Vec<ChatMessage> {
    let mut messages = Vec::new();

    for item in input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let parts: Vec<Value> = content
                    .iter()
                    .filter_map(convert_content_item_to_value)
                    .collect();
                if parts.is_empty() {
                    continue;
                }
                // Collapse a single text part to a plain string for cleanliness.
                let content_value = if parts.len() == 1 {
                    if let Some(text) = parts[0].get("text").and_then(Value::as_str) {
                        Value::String(text.to_string())
                    } else {
                        Value::Array(parts)
                    }
                } else {
                    Value::Array(parts)
                };
                // Chat completions accepts "system", "user", "assistant".
                // Map OpenAI "developer" to "system", anything else to "user".
                let chat_role = match role.as_str() {
                    "assistant" => "assistant",
                    "system" | "developer" => "system",
                    _ => "user",
                };
                messages.push(ChatMessage {
                    role: chat_role.to_string(),
                    content: Some(content_value),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }

            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                provider_metadata,
                ..
            } => {
                // Assistant turn that issued a tool call.
                let mut tool_call = serde_json::json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments,
                    }
                });
                if let Some(metadata) = provider_metadata {
                    tool_call["extra_content"] = metadata.clone();
                }
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(Value::Array(vec![tool_call])),
                    tool_call_id: None,
                    name: None,
                });
            }

            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            }
            | ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let content_text = crate::common::function_output_text(&output.body);
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(Value::String(content_text)),
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                    name: None,
                });
            }

            ResponseItem::LocalShellCall {
                call_id, action, ..
            } => {
                let cmd = match action {
                    chaos_ipc::models::LocalShellAction::Exec(exec) => exec.command.join(" "),
                };
                if let Some(id) = call_id {
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: None,
                        tool_calls: Some(serde_json::json!([{
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": "shell",
                                "arguments": format!(r#"{{"command": {:?}}}"#, cmd),
                            }
                        }])),
                        tool_call_id: None,
                        name: None,
                    });
                }
            }

            // Reasoning items have no chat completions equivalent.
            ResponseItem::Reasoning { .. } => {}

            _ => {
                tracing::debug!(
                    "ChatCompletions adapter: skipping unsupported ResponseItem variant in history"
                );
            }
        }
    }

    messages
}

pub(super) fn convert_tools(tools: &[chaos_abi::ToolDef]) -> Vec<ChatTool> {
    tools
        .iter()
        .map(|tool| match tool {
            chaos_abi::ToolDef::Function(f) => ChatTool {
                tool_type: "function".to_string(),
                function: ChatToolFunction {
                    name: f.name.clone(),
                    description: Some(f.description.clone()),
                    parameters: f.parameters.clone(),
                },
            },
            chaos_abi::ToolDef::Freeform(f) => ChatTool {
                tool_type: "function".to_string(),
                function: ChatToolFunction {
                    name: f.name.clone(),
                    description: Some(format!("{}\n\nFormat: {}", f.description, f.definition)),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "input": {
                                "type": "string",
                                "description": format!(
                                    "Input in {} format: {}",
                                    f.format_type, f.syntax
                                ),
                            }
                        },
                        "required": ["input"],
                    }),
                },
            },
        })
        .collect()
}
