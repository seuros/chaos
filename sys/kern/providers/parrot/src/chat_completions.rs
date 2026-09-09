//! Chat Completions API adapter.
//!
//! Translates chaos-abi `TurnRequest` into OpenAI's `/v1/chat/completions`
//! wire format and streams `TurnEvent`s back from the response.
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
use chaos_libration::UsageSniffer;
use rama::error::BoxError;
use rama::futures::StreamExt;
use rama::http::HeaderMap;
use rama::http::body::util::BodyExt;
use rama::http::sse::EventStream;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
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
    sniffer: Option<Arc<UsageSniffer>>,
}

impl ChatCompletionsAdapter {
    /// Create an adapter backed by a fully-configured provider.
    pub fn new(provider: Provider, api_key: String, default_model: Option<String>) -> Self {
        Self {
            provider,
            api_key,
            default_model,
            sniffer: None,
        }
    }

    /// Attach an optional ration sniffer that records rate-limit
    /// headers for every response this adapter issues.
    pub fn with_sniffer(mut self, sniffer: Option<Arc<UsageSniffer>>) -> Self {
        self.sniffer = sniffer;
        self
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

    fn model_for_request(&self, request_model: &str) -> Result<String, AbiError> {
        if !request_model.is_empty() {
            return Ok(request_model.to_string());
        }
        self.default_model
            .clone()
            .ok_or_else(|| AbiError::InvalidRequest {
                message: "no model configured for Chat Completions provider".to_string(),
            })
    }

    fn build_headers(&self, stream: bool) -> Result<HeaderMap, AbiError> {
        let mut headers = self.provider.headers.clone();
        crate::http_helpers::insert_bearer_auth(&mut headers, &self.api_key, "Chat Completions")?;
        if stream {
            crate::http_helpers::insert_streaming_json_headers(&mut headers);
        } else {
            crate::http_helpers::insert_json_headers(&mut headers);
        }
        Ok(headers)
    }
}

impl ModelAdapter for ChatCompletionsAdapter {
    fn stream(&self, request: TurnRequest) -> AdapterFuture<'_> {
        Box::pin(async move {
            let url = self.chat_completions_url();
            let model = self.model_for_request(&request.model)?;
            let structured = request.output_schema.is_some();
            let body = build_request_body(&request, &model, !structured)?;
            let headers = self.build_headers(!structured)?;
            let provider = self.provider.clone();
            let sniffer = self.sniffer.clone();

            let (tx, rx) = mpsc::channel(64);

            tokio::spawn(async move {
                let result = run_response(
                    &url,
                    &headers,
                    &body,
                    &provider,
                    sniffer.as_ref(),
                    tx.clone(),
                )
                .await;
                if let Err(e) = result {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
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
        ContentItem::Document { name, text, .. } => {
            let header = name
                .as_deref()
                .map(|n| format!("[{n}]\n"))
                .unwrap_or_default();
            Some(serde_json::json!({ "type": "text", "text": format!("{header}{text}") }))
        }
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
    provider_metadata: Option<Value>,
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
/// {"id":"chatcmpl-...","object":"chat.completion.chunk","model":"example-model",
///  "choices":[{"delta":{"role":"assistant","content":"...","tool_calls":[...]},"finish_reason":null}],
///  "usage":null}
/// ```
/// The final chunk carries `"finish_reason": "stop"` or similar, and when
/// `stream_options.include_usage=true` the very last chunk has `"usage"`.
/// Build a `Completed` event from a chat-completions `usage` object,
/// mapping `prompt_tokens`/`completion_tokens` to the token-usage totals.
fn completed_event_from_usage(response_id: &str, usage: &Value) -> TurnEvent {
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    TurnEvent::Completed {
        response_id: response_id.to_string(),
        token_usage: Some(TokenUsage {
            input_tokens: input_tokens as i64,
            output_tokens: output_tokens as i64,
            total_tokens: usage
                .get("total_tokens")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| (input_tokens as i64).saturating_add(output_tokens as i64)),
            ..Default::default()
        }),
    }
}

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
        Some(c) if !c.is_empty() => c,
        _ => {
            // Usage-only trailing chunk.
            if let Some(usage) = json.get("usage").filter(|usage| usage.is_object()) {
                events.push(completed_event_from_usage(response_id, usage));
            }
            return Ok(events);
        }
    };

    for choice in choices {
        let delta = match choice.get("delta").or_else(|| choice.get("message")) {
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
            for (position, tc) in tool_calls.iter().enumerate() {
                let index = tc
                    .get("index")
                    .and_then(Value::as_u64)
                    .map_or(position, |index| index as usize);
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

                if let Some(extra_content) = tc.get("extra_content") {
                    acc.provider_metadata = Some(extra_content.clone());
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
                    provider_metadata: acc.provider_metadata,
                }));
            }

            // Emit completion when not deferring to usage chunk.
            // If `include_usage=true` a separate usage chunk follows; we skip
            // the completion here and let the usage chunk emit it instead.
            // However if `usage` is co-located in this chunk, emit now.
            if let Some(usage) = json.get("usage").filter(|usage| usage.is_object()) {
                events.push(completed_event_from_usage(response_id, usage));
            }
        }
    }

    Ok(events)
}

// ── SSE transport (rama) ───────────────────────────────────────────

async fn run_response(
    url: &str,
    headers: &HeaderMap,
    body: &Value,
    provider: &Provider,
    sniffer: Option<&Arc<UsageSniffer>>,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError> {
    let response = crate::sse::transport::start_rama_post_sse_request(
        url,
        headers,
        body,
        provider,
        "chat_completions",
        sniffer,
    )
    .await?;
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        process_sse_data_stream(
            response.into_body().into_data_stream(),
            provider.stream_idle_timeout,
            tx,
        )
        .await
    } else {
        process_json_response(response.into_body(), provider.stream_idle_timeout, tx).await
    }
}

async fn process_json_response(
    body: rama::http::Body,
    response_timeout: Duration,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError> {
    let bytes = timeout(response_timeout, body.collect())
        .await
        .map_err(|_| AbiError::Stream("timeout waiting for JSON response".to_string()))?
        .map_err(|e| AbiError::Transport {
            status: 0,
            message: e.to_string(),
        })?
        .to_bytes();

    let json = serde_json::from_slice::<Value>(&bytes)
        .map_err(|e| AbiError::Stream(format!("chat completions returned invalid JSON: {e}")))?;

    for event in parse_completion(&json)? {
        if tx.send(Ok(event)).await.is_err() {
            break;
        }
    }
    Ok(())
}

fn parse_completion(json: &Value) -> Result<Vec<TurnEvent>, AbiError> {
    let choices = json
        .get("choices")
        .and_then(Value::as_array)
        .filter(|choices| choices.len() == 1)
        .ok_or_else(|| AbiError::Stream("expected one chat completion choice".to_string()))?;
    let choice = &choices[0];
    let message = choice
        .get("message")
        .filter(|message| message.is_object())
        .ok_or_else(|| AbiError::Stream("chat completion is missing its message".to_string()))?;
    if message
        .get("refusal")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return Err(AbiError::Stream(
            "chat completion refused structured output".to_string(),
        ));
    }
    match choice.get("finish_reason").and_then(Value::as_str) {
        Some("stop" | "tool_calls") => {}
        reason => {
            return Err(AbiError::Stream(format!(
                "incomplete chat completion: {reason:?}"
            )));
        }
    }
    if !message
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
        && !message
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
    {
        return Err(AbiError::Stream(
            "chat completion has no text or tool calls".to_string(),
        ));
    }
    let mut response_id = String::new();
    let mut events = parse_chunk(
        json,
        &mut BTreeMap::new(),
        &mut None,
        &mut response_id,
        &mut None,
    )?;
    if !events
        .iter()
        .any(|event| matches!(event, TurnEvent::Completed { .. }))
    {
        events.push(TurnEvent::Completed {
            response_id,
            token_usage: None,
        });
    }
    Ok(events)
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
    use chaos_test_fixtures::TEST_MODEL;

    fn completion_fixture() -> Value {
        serde_json::json!({
            "id": "chatcmpl-test",
            "model": "server-model",
            "choices": [{
                "message": {"role": "assistant", "content": "{\"ok\":true}"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 17}
        })
    }

    #[test]
    fn complete_json_preserves_lifecycle_and_usage() {
        let events = parse_completion(&completion_fixture()).unwrap();
        assert!(matches!(events.as_slice(), [
            TurnEvent::Created,
            TurnEvent::ServerModel(model),
            TurnEvent::OutputItemAdded(ResponseItem::Message { .. }),
            TurnEvent::OutputTextDelta(text),
            TurnEvent::OutputItemDone(ResponseItem::Message { content, .. }),
            TurnEvent::Completed { response_id, token_usage: Some(usage) },
        ] if model == "server-model"
            && text == "{\"ok\":true}"
            && matches!(content.as_slice(), [ContentItem::OutputText { text }] if text == "{\"ok\":true}")
            && response_id == "chatcmpl-test"
            && usage.input_tokens == 10
            && usage.output_tokens == 5
            && usage.total_tokens == 17));
    }

    #[test]
    fn complete_json_preserves_multiple_tool_calls_without_indexes() {
        let mut json = completion_fixture();
        json["choices"][0] = serde_json::json!({
            "message": {
                "content": null,
                "tool_calls": [
                    {"id": "a", "type": "function", "function": {"name": "first", "arguments": "{\"x\":1}"},
                     "extra_content": {"google": {"thought_signature": "signature"}}},
                    {"id": "b", "type": "function", "function": {"name": "second", "arguments": "{}"}}
                ]
            },
            "finish_reason": "tool_calls"
        });
        json["usage"] = Value::Null;
        let events = parse_completion(&json).unwrap();
        assert!(matches!(events.as_slice(), [
            TurnEvent::Created,
            TurnEvent::ServerModel(_),
            TurnEvent::OutputItemDone(ResponseItem::FunctionCall { call_id: first_id, name: first_name, arguments, provider_metadata: Some(metadata), .. }),
            TurnEvent::OutputItemDone(ResponseItem::FunctionCall { call_id: second_id, name: second_name, .. }),
            TurnEvent::Completed { token_usage: None, .. },
        ] if first_id == "a" && first_name == "first" && arguments == "{\"x\":1}"
            && metadata["google"]["thought_signature"] == "signature"
            && second_id == "b" && second_name == "second"));
    }

    #[test]
    fn complete_json_handles_absent_usage_and_missing_total() {
        let mut json = completion_fixture();
        json["usage"]
            .as_object_mut()
            .unwrap()
            .remove("total_tokens");
        assert!(matches!(parse_completion(&json).unwrap().last(),
            Some(TurnEvent::Completed { token_usage: Some(usage), .. }) if usage.total_tokens == 15));
        json.as_object_mut().unwrap().remove("usage");
        assert!(matches!(
            parse_completion(&json).unwrap().last(),
            Some(TurnEvent::Completed {
                token_usage: None,
                ..
            })
        ));
    }

    #[test]
    fn complete_json_rejects_invalid_refused_and_incomplete_responses() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"error": {"message": "failed"}}),
            serde_json::json!({"choices": []}),
            serde_json::json!({"choices": [{"finish_reason": "stop"}]}),
        ] {
            assert!(parse_completion(&value).is_err(), "{value}");
        }
        for reason in [
            Value::Null,
            serde_json::json!("length"),
            serde_json::json!("content_filter"),
        ] {
            let mut json = completion_fixture();
            json["choices"][0]["finish_reason"] = reason;
            assert!(parse_completion(&json).is_err());
        }
        let mut json = completion_fixture();
        json["choices"][0]["message"]["refusal"] = serde_json::json!("Cannot comply");
        assert!(parse_completion(&json).is_err());
        json["choices"][0]["message"] = serde_json::json!({"content": null});
        assert!(parse_completion(&json).is_err());
    }

    #[tokio::test]
    async fn json_body_errors_do_not_emit_success() {
        let (tx, mut rx) = mpsc::channel(16);
        let result = process_json_response(
            rama::http::Body::from("not json"),
            Duration::from_secs(1),
            tx,
        )
        .await;
        assert!(matches!(result, Err(AbiError::Stream(_))));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn adapter_selects_json_for_schema_and_sse_otherwise() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for structured in [true, false] {
            let server = MockServer::start().await;
            let response = if structured {
                ResponseTemplate::new(200).set_body_json(completion_fixture())
            } else {
                ResponseTemplate::new(200).set_body_raw(
                    concat!(
                        "data: {\"id\":\"chatcmpl-test\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":\"stop\"}],\"usage\":null}\n\n",
                        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":17}}\n\n",
                        "data: [DONE]\n\n",
                    ),
                    "text/event-stream",
                )
            };
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .and(header("authorization", "Bearer secret"))
                .and(header("content-type", "application/json"))
                .and(header(
                    "accept",
                    if structured {
                        "application/json"
                    } else {
                        "text/event-stream"
                    },
                ))
                .and(body_partial_json(
                    serde_json::json!({"stream": !structured}),
                ))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            let adapter = ChatCompletionsAdapter::from_base_url_and_api_key(
                server.uri(),
                "secret".into(),
                None,
            );
            let request = TurnRequest {
                model: "test-model".into(),
                instructions: String::new(),
                input: vec![],
                tools: vec![],
                parallel_tool_calls: false,
                reasoning: None,
                output_schema: structured.then(|| serde_json::json!({"type": "object"})),
                verbosity: None,
                turn_state: None,
                extensions: Default::default(),
            };
            let mut stream = adapter.stream(request).await.unwrap();
            let mut events = Vec::new();
            while let Some(event) = timeout(Duration::from_secs(5), stream.rx_event.recv())
                .await
                .unwrap()
            {
                events.push(event.unwrap());
            }
            assert!(matches!(events.first(), Some(TurnEvent::Created)));
            assert!(matches!(events.last(),
                Some(TurnEvent::Completed { token_usage: Some(usage), .. }) if usage.total_tokens == 17));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, TurnEvent::Completed { .. }))
                    .count(),
                1
            );
            let requests = server.received_requests().await.unwrap();
            let body: Value = requests[0].body_json().unwrap();
            assert_eq!(body.get("stream_options").is_none(), structured);
            assert_eq!(body.get("response_format").is_some(), structured);
        }
    }

    #[test]
    fn json_headers_preserve_custom_headers_and_replace_accept() {
        let mut adapter = ChatCompletionsAdapter::from_base_url_and_api_key(
            "http://localhost".into(),
            "secret".into(),
            None,
        );
        adapter
            .provider
            .headers
            .insert("x-custom", "value".parse().unwrap());
        adapter
            .provider
            .headers
            .insert("accept", "text/event-stream".parse().unwrap());
        let headers = adapter.build_headers(false).unwrap();
        assert_eq!(headers["authorization"], "Bearer secret");
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(headers["accept"], "application/json");
        assert_eq!(headers["x-custom"], "value");
    }

    #[test]
    fn build_request_body_preserves_output_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"verdict": {"type": "string"}},
            "required": ["verdict"],
            "additionalProperties": false
        });
        let mut request = TurnRequest {
            model: "test-model".to_string(),
            instructions: String::new(),
            input: vec![],
            tools: vec![],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: Some(schema.clone()),
            verbosity: None,
            turn_state: None,
            extensions: serde_json::Map::new(),
        };

        let body = build_request_body(&request, "test-model", false).expect("body should build");
        assert_eq!(body["stream"], false);
        assert!(body.get("stream_options").is_none());
        assert_eq!(
            body["response_format"],
            serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "codex_output_schema",
                    "strict": true,
                    "schema": schema
                }
            })
        );
        request.output_schema = None;
        let body = build_request_body(&request, "test-model", true).expect("body should build");
        assert!(body.get("response_format").is_none());
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn build_request_body_includes_system_message() {
        let request = TurnRequest {
            model: TEST_MODEL.to_string(),
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

        let body = build_request_body(&request, TEST_MODEL, true).expect("body should build");
        let messages = body.get("messages").and_then(Value::as_array).unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful");
    }

    #[test]
    fn build_request_body_no_system_when_no_instructions() {
        let request = TurnRequest {
            model: TEST_MODEL.to_string(),
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

        let body = build_request_body(&request, TEST_MODEL, true).expect("body should build");
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
                "model": TEST_MODEL,
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
                "model": TEST_MODEL,
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
                "model": TEST_MODEL,
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
                "model": TEST_MODEL,
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

    #[test]
    fn gemini_thought_signature_survives_tool_call_round_trip() {
        let signature = "opaque-gemini-thought-signature";
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = None;
        let mut response_id = String::new();
        let mut server_model = None;

        parse_chunk(
            &serde_json::json!({
                "id": "chatcmpl-gemini-1",
                "model": "gemini-2.5-pro",
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_exec",
                            "type": "function",
                            "function": {
                                "name": "default_api:exec_command",
                                "arguments": "{\"cmd\":\"ls\"}"
                            },
                            "extra_content": {
                                "google": {
                                    "thought_signature": signature
                                }
                            }
                        }]
                    },
                    "finish_reason": null
                }]
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("Gemini tool-call chunk should parse");

        let events = parse_chunk(
            &serde_json::json!({
                "id": "chatcmpl-gemini-1",
                "model": "gemini-2.5-pro",
                "choices": [{
                    "delta": {},
                    "finish_reason": "tool_calls"
                }]
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("Gemini tool call should finalize");

        let tool_call = events
            .into_iter()
            .find_map(|event| match event {
                TurnEvent::OutputItemDone(item @ ResponseItem::FunctionCall { .. }) => Some(item),
                _ => None,
            })
            .expect("finalized tool call should be emitted");

        // The provider adapter crosses the ABI/IPC boundary before the next
        // request, so exercise serde as well as the in-memory conversion.
        let tool_call: ResponseItem = serde_json::from_value(
            serde_json::to_value(tool_call).expect("tool call should serialize"),
        )
        .expect("tool call should deserialize");

        let request = TurnRequest {
            model: "gemini-2.5-pro".to_string(),
            instructions: String::new(),
            input: vec![
                tool_call,
                ResponseItem::FunctionCallOutput {
                    call_id: "call_exec".to_string(),
                    output: chaos_ipc::models::FunctionCallOutputPayload::from_text(
                        "command output".to_string(),
                    ),
                    tool_name: Some("default_api:exec_command".to_string()),
                },
            ],
            tools: vec![],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions: serde_json::Map::new(),
        };

        let body = build_request_body(&request, "gemini-2.5-pro", true)
            .expect("follow-up body should build");

        assert_eq!(
            body.pointer("/messages/0/tool_calls/0/extra_content/google/thought_signature"),
            Some(&Value::String(signature.to_string())),
            "Gemini requires the original thought signature on the replayed assistant tool call"
        );
        assert_eq!(
            body.pointer("/messages/1/tool_call_id"),
            Some(&Value::String("call_exec".to_string()))
        );
    }

    #[tokio::test]
    async fn process_sse_stream_completes_on_done_without_usage() {
        let stream = futures::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::from(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "id": "chatcmpl-1",
                    "model": TEST_MODEL,
                    "choices": [{"delta": {"content": "Hello"}, "finish_reason": null}]
                })
            ))),
            Ok::<_, std::io::Error>(Bytes::from(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "id": "chatcmpl-1",
                    "model": TEST_MODEL,
                    "choices": [{"delta": {}, "finish_reason": "stop"}]
                })
            ))),
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
        assert!(adapter.build_headers(true).is_err());
        assert!(adapter.build_headers(false).is_err());
    }

    #[test]
    fn build_headers_includes_bearer_token() {
        let adapter = ChatCompletionsAdapter::from_base_url_and_api_key(
            "https://api.openai.com/v1".to_string(),
            "sk-test".to_string(),
            None,
        );
        let headers = adapter.build_headers(true).expect("headers should build");
        assert_eq!(
            headers
                .get(rama::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer sk-test")
        );
    }
}
