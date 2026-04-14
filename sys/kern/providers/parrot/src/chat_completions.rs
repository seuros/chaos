//! Chat Completions API adapter.
//!
//! Translates chaos-abi `TurnRequest` into OpenAI's `/v1/chat/completions`
//! wire format and streams `TurnEvent`s back from the SSE response.
//!
//! HTTP/SSE only — no WebSocket, no sticky routing, no incremental request
//! reuse.  Each follow-up sends full conversation history.

#![warn(clippy::all)]

use crate::provider::Provider;
use bytes::Bytes;
use chaos_abi::AbiError;
use chaos_abi::AdapterFuture;
use chaos_abi::ContentItem;
use chaos_abi::ModelAdapter;
use chaos_abi::ResponseItem;
use chaos_abi::TokenUsage;
use chaos_abi::TurnEvent;
use chaos_abi::TurnRequest;
use chaos_abi::TurnStream;
use http::HeaderMap;
use rama::error::BoxError;
use rama::futures::StreamExt;
use rama::http::sse::EventStream;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Adapter for the OpenAI Chat Completions API (`/v1/chat/completions`).
///
/// Holds a provider (headers, base URL, retry config) and a resolved API key.
/// The API key must be non-empty; it is passed as `Authorization: Bearer <key>`.
#[derive(Debug, Clone)]
pub struct ChatCompletionsAdapter {
    provider: Provider,
    api_key: String,
    default_model: Option<String>,
}

impl ChatCompletionsAdapter {
    /// Create an adapter backed by a fully-configured provider.
    pub fn new(provider: Provider, api_key: String, default_model: Option<String>) -> Self {
        Self {
            provider,
            api_key,
            default_model,
        }
    }

    /// Convenience constructor for standalone use (tests, `adapter_for_wire`).
    pub fn from_base_url_and_api_key(
        base_url: String,
        api_key: String,
        default_model: Option<String>,
    ) -> Self {
        let provider = Provider::from_base_url_with_default_streaming_config(
            "ChatCompletions",
            base_url,
            false,
        );
        Self::new(provider, api_key, default_model)
    }

    fn chat_completions_url(&self) -> String {
        self.provider.url_for_path("/chat/completions")
    }

    fn model_for_request(&self, request_model: &str) -> String {
        if request_model.is_empty() {
            self.default_model
                .clone()
                .unwrap_or_else(|| "gpt-4o".to_string())
        } else {
            request_model.to_string()
        }
    }

    fn build_headers(&self) -> Result<HeaderMap, AbiError> {
        let mut headers = self.provider.headers.clone();

        if self.api_key.trim().is_empty() {
            return Err(AbiError::InvalidRequest {
                message: "Chat Completions provider requires a non-empty API key".to_string(),
            });
        }

        let bearer = format!("Bearer {}", self.api_key);
        let value =
            http::HeaderValue::from_str(&bearer).map_err(|err| AbiError::InvalidRequest {
                message: format!("invalid Authorization header value: {err}"),
            })?;
        headers.insert(http::header::AUTHORIZATION, value);
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static(crate::common::MIME_APPLICATION_JSON),
        );
        headers.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static(crate::common::MIME_TEXT_EVENT_STREAM),
        );
        Ok(headers)
    }
}

impl ModelAdapter for ChatCompletionsAdapter {
    fn stream(&self, request: TurnRequest) -> AdapterFuture<'_> {
        Box::pin(async move {
            let url = self.chat_completions_url();
            let model = self.model_for_request(&request.model);
            let body = build_request_body(&request, &model)?;
            let headers = self.build_headers()?;
            let retry = self.provider.retry.clone();
            let idle_timeout = self.provider.stream_idle_timeout;

            let (tx, rx) = mpsc::channel(64);

            tokio::spawn(async move {
                if let Err(e) =
                    run_sse_stream(&url, &headers, &body, &retry, idle_timeout, tx.clone()).await
                {
                    let _ = tx.send(Err(e)).await;
                }
            });

            Ok(TurnStream { rx_event: rx })
        })
    }

    fn provider_name(&self) -> &str {
        "ChatCompletions"
    }

    fn capabilities(&self) -> chaos_abi::AdapterCapabilities {
        chaos_abi::AdapterCapabilities {
            can_list_models: false,
        }
    }
}

// ── Request building ───────────────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    stream_options: StreamOptions,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatMessage {
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
struct ChatTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ChatToolFunction,
}

#[derive(Serialize)]
struct ChatToolFunction {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    parameters: Value,
}

fn build_request_body(request: &TurnRequest, model: &str) -> Result<Value, AbiError> {
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

    let tools = convert_tools(&request.tools);
    let parallel_tool_calls = if request.tools.is_empty() {
        None
    } else {
        Some(request.parallel_tool_calls)
    };

    let body = serde_json::to_value(ChatRequest {
        model: model.to_string(),
        stream: true,
        messages,
        tools,
        parallel_tool_calls,
        stream_options: StreamOptions {
            include_usage: true,
        },
    })
    .map_err(|e| AbiError::InvalidRequest {
        message: e.to_string(),
    })?;

    Ok(body)
}

fn convert_content_item_to_value(c: &ContentItem) -> Option<Value> {
    match c {
        ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
            Some(serde_json::json!({"type": "text", "text": text}))
        }
        ContentItem::InputImage { image_url } => Some(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": image_url }
        })),
    }
}

fn convert_input_to_messages(input: &[ResponseItem]) -> Vec<ChatMessage> {
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
                ..
            } => {
                // Assistant turn that issued a tool call.
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(serde_json::json!([{
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments,
                        }
                    }])),
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
                            "function": { "name": "shell", "arguments": format!(r#"{{"command": {:?}}}"#, cmd) }
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

fn convert_tools(tools: &[chaos_abi::ToolDef]) -> Vec<ChatTool> {
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
                                "description": format!("Input in {} format: {}", f.format_type, f.syntax),
                            }
                        },
                        "required": ["input"],
                    }),
                },
            },
        })
        .collect()
}

// ── SSE event parsing ──────────────────────────────────────────────

/// In-flight tool call accumulator for a single tool_calls index.
#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

/// In-flight text accumulator so we can finalize the assistant message.
#[derive(Default)]
struct TextAccumulator {
    text: String,
}

/// Parse a single `data: <json>` line from the Chat Completions SSE stream.
///
/// The schema is:
/// ```json
/// {"id":"chatcmpl-...","object":"chat.completion.chunk","model":"gpt-4o",
///  "choices":[{"delta":{"role":"assistant","content":"...","tool_calls":[...]},"finish_reason":null}],
///  "usage":null}
/// ```
/// The final chunk carries `"finish_reason": "stop"` or similar, and when
/// `stream_options.include_usage=true` the very last chunk has `"usage"`.
fn parse_chunk(
    json: &Value,
    tool_acc: &mut BTreeMap<usize, ToolCallAccumulator>,
    text_acc: &mut Option<TextAccumulator>,
    response_id: &mut String,
    server_model: &mut Option<String>,
) -> Result<Vec<TurnEvent>, AbiError> {
    let mut events = Vec::new();

    // Capture the response id and model from the first chunk.
    if let Some(id) = json.get("id").and_then(Value::as_str)
        && response_id.is_empty()
    {
        *response_id = id.to_string();
        events.push(TurnEvent::Created);
    }
    if let Some(model) = json.get("model").and_then(Value::as_str)
        && server_model.is_none()
    {
        *server_model = Some(model.to_string());
        events.push(TurnEvent::ServerModel(model.to_string()));
    }

    let choices = match json.get("choices").and_then(Value::as_array) {
        Some(c) => c,
        None => {
            // Usage-only trailing chunk.
            if let Some(usage) = json.get("usage") {
                let input_tokens = usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                events.push(TurnEvent::Completed {
                    response_id: response_id.clone(),
                    token_usage: Some(TokenUsage {
                        input_tokens: input_tokens as i64,
                        output_tokens: output_tokens as i64,
                        total_tokens: (input_tokens + output_tokens) as i64,
                        ..Default::default()
                    }),
                });
            }
            return Ok(events);
        }
    };

    for choice in choices {
        let delta = match choice.get("delta") {
            Some(d) => d,
            None => continue,
        };
        let finish_reason = choice.get("finish_reason").and_then(Value::as_str);

        // Text delta
        if let Some(content) = delta.get("content").and_then(Value::as_str)
            && !content.is_empty()
        {
            if text_acc.is_none() {
                events.push(TurnEvent::OutputItemAdded(ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![],
                    phase: None,
                    end_turn: None,
                }));
                *text_acc = Some(TextAccumulator::default());
            }
            if let Some(acc) = text_acc.as_mut() {
                acc.text.push_str(content);
            }
            events.push(TurnEvent::OutputTextDelta(content.to_string()));
        }

        // Tool call deltas
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tool_calls {
                let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let acc = tool_acc.entry(index).or_default();

                if let Some(id) = tc.get("id").and_then(Value::as_str) {
                    acc.id = id.to_string();
                }

                if let Some(name) = tc.pointer("/function/name").and_then(Value::as_str) {
                    acc.name = name.to_string();
                }

                if let Some(chunk) = tc.pointer("/function/arguments").and_then(Value::as_str) {
                    acc.arguments.push_str(chunk);
                }
            }
        }

        // Finish — close any in-flight tool call and/or text block.
        if finish_reason.is_some() {
            if let Some(acc) = text_acc.take() {
                events.push(TurnEvent::OutputItemDone(ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText { text: acc.text }],
                    phase: None,
                    end_turn: None,
                }));
            }

            for (_, acc) in std::mem::take(tool_acc) {
                events.push(TurnEvent::OutputItemDone(ResponseItem::FunctionCall {
                    id: None,
                    name: acc.name,
                    arguments: if acc.arguments.is_empty() {
                        "{}".to_string()
                    } else {
                        acc.arguments
                    },
                    call_id: acc.id,
                    namespace: None,
                }));
            }

            // Emit completion when not deferring to usage chunk.
            // If `include_usage=true` a separate usage chunk follows; we skip
            // the completion here and let the usage chunk emit it instead.
            // However if `usage` is co-located in this chunk, emit now.
            if let Some(usage) = json.get("usage") {
                let input_tokens = usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                events.push(TurnEvent::Completed {
                    response_id: response_id.clone(),
                    token_usage: Some(TokenUsage {
                        input_tokens: input_tokens as i64,
                        output_tokens: output_tokens as i64,
                        total_tokens: (input_tokens + output_tokens) as i64,
                        ..Default::default()
                    }),
                });
            }
        }
    }

    Ok(events)
}

// ── SSE transport (rama) ───────────────────────────────────────────

async fn run_sse_stream(
    url: &str,
    headers: &HeaderMap,
    body: &Value,
    retry: &crate::provider::RetryConfig,
    idle_timeout: Duration,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError> {
    let response = crate::sse::transport::start_rama_post_sse_request(
        url,
        headers,
        body,
        retry,
        "chat_completions",
        None,
    )
    .await?;
    process_sse_data_stream(response.into_body().into_data_stream(), idle_timeout, tx).await
}

async fn process_sse_data_stream<S, E>(
    data_stream: S,
    idle_timeout: Duration,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<BoxError> + std::fmt::Display,
{
    let mut tool_acc: BTreeMap<usize, ToolCallAccumulator> = BTreeMap::new();
    let mut text_acc: Option<TextAccumulator> = None;
    let mut response_id = String::new();
    let mut server_model: Option<String> = None;
    let mut stream = EventStream::<_, String>::new(data_stream);
    let mut completed_emitted = false;

    loop {
        let sse = match timeout(idle_timeout, stream.next()).await {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(err))) => return Err(AbiError::Stream(err.to_string())),
            Ok(None) => break,
            Err(_) => return Err(AbiError::Stream("idle timeout waiting for SSE".to_string())),
        };
        let data = match sse.data() {
            Some(data) => data.trim(),
            None => continue,
        };

        if data == "[DONE]" {
            if !completed_emitted {
                let _ = tx
                    .send(Ok(TurnEvent::Completed {
                        response_id: response_id.clone(),
                        token_usage: None,
                    }))
                    .await;
            }
            return Ok(());
        }

        let json = match serde_json::from_str::<Value>(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let events = parse_chunk(
            &json,
            &mut tool_acc,
            &mut text_acc,
            &mut response_id,
            &mut server_model,
        )?;

        for event in events {
            let is_done = matches!(&event, TurnEvent::Completed { .. });
            if is_done {
                completed_emitted = true;
            }
            if tx.send(Ok(event)).await.is_err() {
                return Ok(());
            }
            if is_done {
                return Ok(());
            }
        }
    }

    // Stream ended without [DONE] — treat as completion with no token data.
    if !completed_emitted {
        let _ = tx
            .send(Ok(TurnEvent::Completed {
                response_id,
                token_usage: None,
            }))
            .await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_body_includes_system_message() {
        let request = TurnRequest {
            model: "gpt-4o".to_string(),
            instructions: "You are helpful".to_string(),
            input: vec![],
            tools: vec![],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions: serde_json::Map::new(),
        };

        let body = build_request_body(&request, "gpt-4o").expect("body should build");
        let messages = body.get("messages").and_then(Value::as_array).unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful");
    }

    #[test]
    fn build_request_body_no_system_when_no_instructions() {
        let request = TurnRequest {
            model: "gpt-4o".to_string(),
            instructions: String::new(),
            input: vec![],
            tools: vec![],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions: serde_json::Map::new(),
        };

        let body = build_request_body(&request, "gpt-4o").expect("body should build");
        let messages = body.get("messages").and_then(Value::as_array);
        assert!(
            messages.is_none(),
            "expected messages to be omitted when empty"
        );
    }

    #[test]
    fn parse_chunk_finalizes_plain_text_message() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = None;
        let mut response_id = String::new();
        let mut server_model = None;

        let start_events = parse_chunk(
            &serde_json::json!({
                "id": "chatcmpl-1",
                "model": "gpt-4o",
                "choices": [{
                    "delta": { "content": "Hello" },
                    "finish_reason": null
                }]
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("chunk should parse");
        assert!(start_events.iter().any(|event| matches!(
            event,
            TurnEvent::OutputItemAdded(ResponseItem::Message { role, .. }) if role == "assistant"
        )));
        assert!(start_events.iter().any(|event| matches!(
            event,
            TurnEvent::OutputTextDelta(delta) if delta == "Hello"
        )));

        let finish_events = parse_chunk(
            &serde_json::json!({
                "id": "chatcmpl-1",
                "model": "gpt-4o",
                "choices": [{
                    "delta": {},
                    "finish_reason": "stop"
                }]
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("chunk should parse");
        assert!(finish_events.iter().any(|event| matches!(
            event,
            TurnEvent::OutputItemDone(ResponseItem::Message { role, content, .. })
                if role == "assistant"
                    && content == &vec![ContentItem::OutputText {
                        text: "Hello".to_string()
                    }]
        )));
    }

    #[test]
    fn parse_chunk_tracks_parallel_tool_calls_by_index() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = None;
        let mut response_id = String::new();
        let mut server_model = None;

        let initial_events = parse_chunk(
            &serde_json::json!({
                "id": "chatcmpl-1",
                "model": "gpt-4o",
                "choices": [{
                    "delta": {
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": "call_1",
                                "function": { "name": "first", "arguments": "{\"a\":" }
                            },
                            {
                                "index": 1,
                                "id": "call_2",
                                "function": { "name": "second", "arguments": "{\"b\":" }
                            }
                        ]
                    },
                    "finish_reason": null
                }]
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("chunk should parse");
        assert!(
            initial_events.is_empty()
                || initial_events
                    .iter()
                    .all(|event| matches!(event, TurnEvent::Created | TurnEvent::ServerModel(_)))
        );

        let finish_events = parse_chunk(
            &serde_json::json!({
                "id": "chatcmpl-1",
                "model": "gpt-4o",
                "choices": [{
                    "delta": {
                        "tool_calls": [
                            {
                                "index": 1,
                                "function": { "arguments": "2}" }
                            },
                            {
                                "index": 0,
                                "function": { "arguments": "1}" }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }]
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("chunk should parse");

        assert_eq!(finish_events.len(), 2);
        assert!(matches!(
            &finish_events[0],
            TurnEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }) if name == "first" && arguments == "{\"a\":1}" && call_id == "call_1"
        ));
        assert!(matches!(
            &finish_events[1],
            TurnEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }) if name == "second" && arguments == "{\"b\":2}" && call_id == "call_2"
        ));
    }

    #[tokio::test]
    async fn process_sse_stream_completes_on_done_without_usage() {
        let stream = futures::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::from(
                "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            )),
            Ok::<_, std::io::Error>(Bytes::from(
                "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            )),
            Ok::<_, std::io::Error>(Bytes::from("data: [DONE]\n\n")),
        ]);
        let (tx, mut rx) = mpsc::channel(16);

        process_sse_data_stream(stream, Duration::from_secs(1), tx)
            .await
            .expect("stream should succeed");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event.expect("event should be ok"));
        }

        assert!(events.iter().any(|event| matches!(
            event,
            TurnEvent::Completed { response_id, token_usage: None } if response_id == "chatcmpl-1"
        )));
    }

    #[test]
    fn build_headers_rejects_empty_api_key() {
        let adapter = ChatCompletionsAdapter::from_base_url_and_api_key(
            "https://api.openai.com/v1".to_string(),
            String::new(),
            None,
        );
        assert!(adapter.build_headers().is_err());
    }

    #[test]
    fn build_headers_includes_bearer_token() {
        let adapter = ChatCompletionsAdapter::from_base_url_and_api_key(
            "https://api.openai.com/v1".to_string(),
            "sk-test".to_string(),
            None,
        );
        let headers = adapter.build_headers().expect("headers should build");
        assert_eq!(
            headers
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer sk-test")
        );
    }
}
