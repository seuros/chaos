//! Anthropic Messages API adapter.
//!
//! Translates chaos-abi `TurnRequest` into Anthropic's `/v1/messages`
//! wire format and streams `TurnEvent`s back from the SSE response.

use chaos_abi::AbiError;
use chaos_abi::AdapterFuture;
use chaos_abi::ContentItem;
use chaos_abi::ModelAdapter;
use chaos_abi::ResponseItem;
use chaos_abi::TokenUsage;
use chaos_abi::TurnEvent;
use chaos_abi::TurnRequest;
use chaos_abi::TurnStream;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;

const DEFAULT_MAX_TOKENS: u64 = 8192;
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Adapter for the Anthropic Messages API.
#[derive(Debug, Clone)]
pub struct AnthropicAdapter {
    base_url: String,
    api_key: String,
    default_model: Option<String>,
}

impl AnthropicAdapter {
    pub fn new(base_url: String, api_key: String, default_model: Option<String>) -> Self {
        Self {
            base_url,
            api_key,
            default_model,
        }
    }

    fn messages_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/messages")
    }

    fn model_for_request(&self, request_model: &str) -> String {
        if request_model.is_empty() {
            self.default_model
                .clone()
                .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string())
        } else {
            request_model.to_string()
        }
    }
}

impl ModelAdapter for AnthropicAdapter {
    fn stream(&self, request: TurnRequest) -> AdapterFuture<'_> {
        Box::pin(async move {
            let url = self.messages_url();
            let model = self.model_for_request(&request.model);
            let body = build_request_body(&request, &model)?;

            let (tx, rx) = mpsc::channel(64);

            let api_key = self.api_key.clone();
            tokio::spawn(async move {
                if let Err(e) = run_sse_stream(&url, &api_key, &body, tx.clone()).await {
                    let _ = tx.send(Err(e)).await;
                }
            });

            Ok(TurnStream { rx_event: rx })
        })
    }

    fn provider_name(&self) -> &str {
        "Anthropic"
    }
}

// ── Request building ───────────────────────────────────────────────

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u64,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: Value,
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    input_schema: Value,
}

fn build_request_body(request: &TurnRequest, model: &str) -> Result<Value, AbiError> {
    let max_tokens = request
        .extensions
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_TOKENS);

    let system = if request.instructions.is_empty() {
        None
    } else {
        Some(request.instructions.clone())
    };

    let messages = convert_input_to_messages(&request.input);
    let tools = convert_tools(&request.tools);

    let mut body = serde_json::to_value(AnthropicRequest {
        model: model.to_string(),
        max_tokens,
        stream: true,
        system,
        messages,
        tools,
    })
    .map_err(|e| AbiError::InvalidRequest {
        message: e.to_string(),
    })?;

    let obj = body
        .as_object_mut()
        .ok_or_else(|| AbiError::InvalidRequest {
            message: "request body is not an object".to_string(),
        })?;

    // parallel_tool_calls → tool_choice.disable_parallel_tool_use (inverted)
    if !request.parallel_tool_calls && !request.tools.is_empty() {
        obj.insert(
            "tool_choice".to_string(),
            serde_json::json!({
                "type": "auto",
                "disable_parallel_tool_use": true,
            }),
        );
    }

    // reasoning → thinking config
    if let Some(ref _reasoning) = request.reasoning {
        if let Some(budget) = request
            .extensions
            .get("thinking_budget_tokens")
            .and_then(|v| v.as_u64())
        {
            obj.insert(
                "thinking".to_string(),
                serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": budget,
                }),
            );
        }
    }

    // output_schema → structured output via tool_choice forced
    if let Some(ref schema) = request.output_schema {
        obj.entry("tools".to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .map(|tools_arr| {
                tools_arr.push(serde_json::json!({
                    "name": "_structured_output",
                    "description": "Return structured output matching the schema",
                    "input_schema": schema,
                }));
            });
        obj.insert(
            "tool_choice".to_string(),
            serde_json::json!({"type": "tool", "name": "_structured_output"}),
        );
    }

    Ok(body)
}

fn convert_content_item(c: &ContentItem) -> Option<Value> {
    match c {
        ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
            Some(serde_json::json!({"type": "text", "text": text}))
        }
        ContentItem::InputImage { image_url } => {
            if let Some(rest) = image_url.strip_prefix("data:") {
                let (media_type, data) = rest.split_once(",").unwrap_or(("image/png;base64", rest));
                let media_type = media_type.strip_suffix(";base64").unwrap_or(media_type);
                Some(serde_json::json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": data }
                }))
            } else {
                Some(serde_json::json!({
                    "type": "image",
                    "source": { "type": "url", "url": image_url }
                }))
            }
        }
    }
}

fn convert_input_to_messages(input: &[ResponseItem]) -> Vec<AnthropicMessage> {
    let mut messages = Vec::new();

    for item in input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let content_value: Vec<Value> =
                    content.iter().filter_map(convert_content_item).collect();
                if !content_value.is_empty() {
                    messages.push(AnthropicMessage {
                        role: role.clone(),
                        content: Value::Array(content_value),
                    });
                }
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                messages.push(AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([{
                        "type": "tool_use",
                        "id": call_id,
                        "name": name,
                        "input": serde_json::from_str::<Value>(arguments)
                            .unwrap_or(Value::Object(Default::default())),
                    }]),
                });
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            }
            | ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let content_text = match &output.body {
                    chaos_ipc::models::FunctionCallOutputBody::Text(text) => text.clone(),
                    chaos_ipc::models::FunctionCallOutputBody::ContentItems(items) => items
                        .iter()
                        .filter_map(|c| match c {
                            chaos_ipc::models::FunctionCallOutputContentItem::InputText {
                                text,
                            } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                messages.push(AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([{
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": content_text,
                    }]),
                });
            }
            ResponseItem::LocalShellCall {
                call_id, action, ..
            } => {
                let cmd = match action {
                    chaos_ipc::models::LocalShellAction::Exec(exec) => exec.command.join(" "),
                };
                if let Some(call_id) = call_id {
                    messages.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: serde_json::json!([{
                            "type": "tool_use",
                            "id": call_id,
                            "name": "shell",
                            "input": {"command": cmd},
                        }]),
                    });
                }
            }
            // Reasoning items are not part of Anthropic's message history
            ResponseItem::Reasoning { .. } => {}
            // Skip items that have no Anthropic equivalent
            _ => {
                tracing::debug!(
                    "Anthropic adapter: skipping unsupported ResponseItem variant in history"
                );
            }
        }
    }

    messages
}

fn convert_tools(tools: &[chaos_abi::ToolDef]) -> Vec<AnthropicTool> {
    tools
        .iter()
        .map(|tool| match tool {
            chaos_abi::ToolDef::Function(f) => AnthropicTool {
                name: f.name.clone(),
                description: Some(f.description.clone()),
                input_schema: f.parameters.clone(),
            },
            chaos_abi::ToolDef::Freeform(f) => {
                // Freeform tools have no JSON Schema — wrap the definition
                // as a single string parameter so Anthropic can still call them.
                AnthropicTool {
                    name: f.name.clone(),
                    description: Some(format!("{}\n\nFormat: {}", f.description, f.definition)),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "input": {
                                "type": "string",
                                "description": format!("Input in {} format: {}", f.format_type, f.syntax),
                            }
                        },
                        "required": ["input"],
                    }),
                }
            }
        })
        .collect()
}

// ── SSE event parsing ──────────────────────────────────────────────

/// In-flight tool_use accumulator. Anthropic streams tool input as
/// `input_json_delta` chunks — we buffer until `content_block_stop`.
#[derive(Default)]
struct ToolUseAccumulator {
    id: String,
    name: String,
    input_json: String,
}

/// Parse a single SSE event block into zero or more TurnEvents.
///
/// Anthropic SSE event lifecycle for a tool call:
///   content_block_start (type=tool_use, id, name)
///   content_block_delta (type=input_json_delta, partial_json) × N
///   content_block_stop  (index)
///
/// For text:
///   content_block_start (type=text)
///   content_block_delta (type=text_delta, text) × N
///   content_block_stop  (index)
///
/// For thinking:
///   content_block_start (type=thinking)
///   content_block_delta (type=thinking_delta, thinking) × N
///   content_block_stop  (index)
fn parse_sse_event(
    event_type: &str,
    json: &Value,
    tool_acc: &mut Option<ToolUseAccumulator>,
) -> Result<Vec<TurnEvent>, AbiError> {
    match event_type {
        "message_start" => {
            let model = json
                .pointer("/message/model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(vec![TurnEvent::Created, TurnEvent::ServerModel(model)])
        }

        "content_block_start" => {
            let block_type = json
                .pointer("/content_block/type")
                .and_then(Value::as_str)
                .unwrap_or("");
            if block_type == "tool_use" {
                let id = json
                    .pointer("/content_block/id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = json
                    .pointer("/content_block/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                *tool_acc = Some(ToolUseAccumulator {
                    id,
                    name,
                    input_json: String::new(),
                });
            }
            Ok(vec![])
        }

        "content_block_delta" => {
            let delta_type = json
                .pointer("/delta/type")
                .and_then(Value::as_str)
                .unwrap_or("");
            match delta_type {
                "text_delta" => {
                    if let Some(text) = json.pointer("/delta/text").and_then(Value::as_str) {
                        Ok(vec![TurnEvent::OutputTextDelta(text.to_string())])
                    } else {
                        Ok(vec![])
                    }
                }
                "thinking_delta" => {
                    if let Some(text) = json.pointer("/delta/thinking").and_then(Value::as_str) {
                        Ok(vec![TurnEvent::ReasoningContentDelta {
                            delta: text.to_string(),
                            content_index: 0,
                        }])
                    } else {
                        Ok(vec![])
                    }
                }
                "input_json_delta" => {
                    if let Some(acc) = tool_acc.as_mut() {
                        if let Some(chunk) =
                            json.pointer("/delta/partial_json").and_then(Value::as_str)
                        {
                            acc.input_json.push_str(chunk);
                        }
                    }
                    Ok(vec![])
                }
                _ => Ok(vec![]),
            }
        }

        "content_block_stop" => {
            // If we were accumulating a tool_use, emit it now
            if let Some(acc) = tool_acc.take() {
                Ok(vec![TurnEvent::OutputItemDone(
                    ResponseItem::FunctionCall {
                        id: None,
                        name: acc.name,
                        arguments: if acc.input_json.is_empty() {
                            "{}".to_string()
                        } else {
                            acc.input_json
                        },
                        call_id: acc.id,
                        namespace: None,
                    },
                )])
            } else {
                Ok(vec![])
            }
        }

        "message_delta" => {
            let stop_reason = json.pointer("/delta/stop_reason").and_then(Value::as_str);
            if stop_reason.is_some() {
                let input_tokens = json
                    .pointer("/usage/input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let output_tokens = json
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                Ok(vec![TurnEvent::Completed {
                    response_id: String::new(),
                    token_usage: Some(TokenUsage {
                        input_tokens: input_tokens as i64,
                        output_tokens: output_tokens as i64,
                        total_tokens: (input_tokens + output_tokens) as i64,
                        ..Default::default()
                    }),
                }])
            } else {
                Ok(vec![])
            }
        }

        "message_stop" | "ping" => Ok(vec![]),

        "error" => {
            let message = json
                .pointer("/error/message")
                .or_else(|| json.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown provider error")
                .to_string();
            let error_type = json
                .pointer("/error/type")
                .and_then(Value::as_str)
                .unwrap_or("error");
            // Map known error types to specific ABI errors
            let err = match error_type {
                "overloaded_error" => AbiError::ServerOverloaded,
                "rate_limit_error" => AbiError::Retryable {
                    message,
                    delay: None,
                },
                "invalid_request_error" if message.contains("context") => {
                    AbiError::ContextWindowExceeded
                }
                _ => AbiError::Stream(format!("{error_type}: {message}")),
            };
            Err(err)
        }

        _ => Ok(vec![]),
    }
}

// ── SSE transport (rama) ───────────────────────────────────────────

async fn run_sse_stream(
    url: &str,
    api_key: &str,
    body: &Value,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError> {
    use rama::Service;
    use rama::http::Body;
    use rama::http::Request;
    use rama::http::StatusCode;
    use rama::http::body::util::BodyExt;
    use rama::http::client::EasyHttpWebClient;

    let body_bytes = serde_json::to_vec(body).map_err(|e| AbiError::InvalidRequest {
        message: e.to_string(),
    })?;

    let client = EasyHttpWebClient::default();
    let request = Request::builder()
        .method("POST")
        .uri(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(Body::from(body_bytes))
        .map_err(|e| AbiError::InvalidRequest {
            message: e.to_string(),
        })?;

    let response = client
        .serve(request)
        .await
        .map_err(|e| AbiError::Transport {
            status: 0,
            message: e.to_string(),
        })?;

    let status = response.status();
    if status != StatusCode::OK {
        let body_bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| AbiError::Transport {
                status: status.as_u16(),
                message: e.to_string(),
            })?
            .to_bytes();
        let body_text = String::from_utf8_lossy(&body_bytes);
        return Err(AbiError::Transport {
            status: status.as_u16(),
            message: body_text.to_string(),
        });
    }

    let mut tool_acc: Option<ToolUseAccumulator> = None;
    let mut buffer = String::new();
    let mut data_stream = response.into_body().into_data_stream();

    use futures::StreamExt;
    while let Some(chunk) = data_stream.next().await {
        let chunk = chunk.map_err(|e| AbiError::Stream(e.to_string()))?;
        let text = String::from_utf8_lossy(&chunk);
        buffer.push_str(&text);

        while let Some(pos) = buffer.find("\n\n") {
            let event_block = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            let mut event_type = None;
            let mut data = None;
            for line in event_block.lines() {
                if let Some(rest) = line.strip_prefix("event: ") {
                    event_type = Some(rest.trim().to_string());
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    data = Some(rest.trim().to_string());
                }
            }

            let Some(event_type) = event_type else {
                continue;
            };
            let Some(data) = data else { continue };
            let Ok(json) = serde_json::from_str::<Value>(&data) else {
                continue;
            };

            let events = match parse_sse_event(&event_type, &json, &mut tool_acc) {
                Ok(events) => events,
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                    return Ok(());
                }
            };
            for event in events {
                let is_done = matches!(&event, TurnEvent::Completed { .. });
                if tx.send(Ok(event)).await.is_err() {
                    return Ok(());
                }
                if is_done {
                    return Ok(());
                }
            }
        }
    }

    Ok(())
}
