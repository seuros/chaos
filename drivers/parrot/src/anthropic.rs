//! Anthropic Messages API adapter.
//!
//! Translates chaos-abi `TurnRequest` into Anthropic's `/v1/messages`
//! wire format and streams `TurnEvent`s back from the SSE response.

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
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

const DEFAULT_MAX_TOKENS: u64 = 8192;
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicAuth {
    ApiKey(String),
    BearerToken(String),
}

/// Adapter for the Anthropic Messages API.
///
/// Uses the provider's headers, retry config, and query params so that
/// kern's provider configuration surface is honored end-to-end.
#[derive(Debug, Clone)]
pub struct AnthropicAdapter {
    provider: Provider,
    auth: AnthropicAuth,
    default_model: Option<String>,
}

impl AnthropicAdapter {
    /// Create an adapter backed by a fully-configured provider.
    pub fn new(provider: Provider, auth: AnthropicAuth, default_model: Option<String>) -> Self {
        Self {
            provider,
            auth,
            default_model,
        }
    }

    /// Convenience constructor for standalone use (tests, adapter_for_wire).
    pub fn from_base_url_and_api_key(
        base_url: String,
        api_key: String,
        default_model: Option<String>,
    ) -> Self {
        use crate::provider::RetryConfig;
        use std::time::Duration;

        let provider = Provider {
            name: "Anthropic".to_string(),
            base_url,
            query_params: None,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 4,
                base_delay: Duration::from_millis(200),
                retry_429: true,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(300),
        };
        Self::new(provider, AnthropicAuth::ApiKey(api_key), default_model)
    }

    fn messages_url(&self) -> String {
        self.provider.url_for_path("/messages")
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

    /// Build the merged header map: provider headers + Anthropic-specific headers.
    fn build_headers(&self) -> Result<HeaderMap, AbiError> {
        let mut headers = self.provider.headers.clone();
        match &self.auth {
            AnthropicAuth::ApiKey(api_key) => {
                if api_key.trim().is_empty() {
                    return Err(AbiError::InvalidRequest {
                        message: "Anthropic Messages requires a non-empty API key".to_string(),
                    });
                }
                let value = http::HeaderValue::from_str(api_key).map_err(|err| {
                    AbiError::InvalidRequest {
                        message: format!("invalid Anthropic API key header value: {err}"),
                    }
                })?;
                headers.insert("x-api-key", value);
            }
            AnthropicAuth::BearerToken(token) => {
                if token.trim().is_empty() {
                    return Err(AbiError::InvalidRequest {
                        message: "Anthropic Messages requires a non-empty bearer token".to_string(),
                    });
                }
                let value =
                    http::HeaderValue::from_str(&format!("Bearer {token}")).map_err(|err| {
                        AbiError::InvalidRequest {
                            message: format!("invalid Anthropic authorization header: {err}"),
                        }
                    })?;
                headers.insert(http::header::AUTHORIZATION, value);
            }
        }
        headers.insert(
            "anthropic-version",
            http::HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static("text/event-stream"),
        );
        Ok(headers)
    }
}

impl ModelAdapter for AnthropicAdapter {
    fn stream(&self, request: TurnRequest) -> AdapterFuture<'_> {
        Box::pin(async move {
            if request.output_schema.is_some() {
                return Err(AbiError::InvalidRequest {
                    message: "structured output (output_schema) is not yet supported for \
                              Anthropic Messages — remove output_schema or use a Responses \
                              provider"
                        .to_string(),
                });
            }

            let url = self.messages_url();
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
        "Anthropic"
    }

    fn capabilities(&self) -> chaos_abi::AdapterCapabilities {
        chaos_abi::AdapterCapabilities {
            can_list_models: true,
        }
    }

    fn list_models(&self) -> chaos_abi::ListModelsFuture<'_> {
        Box::pin(async {
            let url = self.provider.url_for_path("/models");
            let headers = self
                .build_headers()
                .map_err(|e| chaos_abi::ListModelsError::Failed {
                    message: e.to_string(),
                })?;

            fetch_anthropic_models(&url, &headers).await
        })
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
        .and_then(Value::as_u64)
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
    if let Some(ref _reasoning) = request.reasoning
        && let Some(budget) = request
            .extensions
            .get("thinking_budget_tokens")
            .and_then(Value::as_u64)
    {
        obj.insert(
            "thinking".to_string(),
            serde_json::json!({
                "type": "enabled",
                "budget_tokens": budget,
            }),
        );
    }

    // output_schema is guarded at the adapter level — reject before we get here.
    // The previous synthetic _structured_output tool was not production-safe.

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
                    // Anthropic only accepts "user" or "assistant". Map OpenAI's
                    // "developer" / "system" roles to "user".
                    let anthropic_role = match role.as_str() {
                        "assistant" => "assistant",
                        _ => "user",
                    };
                    messages.push(AnthropicMessage {
                        role: anthropic_role.to_string(),
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

/// In-flight text accumulator. We buffer text deltas so we can emit
/// a complete `OutputItemDone(Message{...})` at `content_block_stop`.
#[derive(Default)]
struct TextAccumulator {
    text: String,
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
    text_acc: &mut Option<TextAccumulator>,
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
            match block_type {
                "tool_use" => {
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
                    Ok(vec![])
                }
                "text" => {
                    // Kern needs OutputItemAdded before any OutputTextDelta.
                    *text_acc = Some(TextAccumulator::default());
                    Ok(vec![TurnEvent::OutputItemAdded(ResponseItem::Message {
                        id: None,
                        role: "assistant".to_string(),
                        content: vec![],
                        phase: None,
                        end_turn: None,
                    })])
                }
                _ => Ok(vec![]),
            }
        }

        "content_block_delta" => {
            let delta_type = json
                .pointer("/delta/type")
                .and_then(Value::as_str)
                .unwrap_or("");
            match delta_type {
                "text_delta" => {
                    if let Some(text) = json.pointer("/delta/text").and_then(Value::as_str) {
                        if let Some(acc) = text_acc.as_mut() {
                            acc.text.push_str(text);
                        }
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
                    if let Some(acc) = tool_acc.as_mut()
                        && let Some(chunk) =
                            json.pointer("/delta/partial_json").and_then(Value::as_str)
                    {
                        acc.input_json.push_str(chunk);
                    }
                    Ok(vec![])
                }
                _ => Ok(vec![]),
            }
        }

        "content_block_stop" => {
            if let Some(acc) = tool_acc.take() {
                // Tool use block finished — emit the completed function call.
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
            } else if let Some(acc) = text_acc.take() {
                // Text block finished — emit the completed message.
                Ok(vec![TurnEvent::OutputItemDone(ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText { text: acc.text }],
                    phase: None,
                    end_turn: None,
                })])
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
    headers: &HeaderMap,
    body: &Value,
    retry: &crate::provider::RetryConfig,
    idle_timeout: Duration,
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

    let max_attempts = retry.max_attempts.max(1);
    let base_delay = retry.base_delay;

    for attempt in 0..max_attempts {
        if attempt > 0 {
            let delay = base_delay * 2u32.saturating_pow(attempt.saturating_sub(1) as u32);
            tokio::time::sleep(delay).await;
        }

        let client = EasyHttpWebClient::default();
        let mut builder = Request::builder().method("POST").uri(url);
        for (name, value) in headers.iter() {
            builder = builder.header(name, value);
        }
        let request =
            builder
                .body(Body::from(body_bytes.clone()))
                .map_err(|e| AbiError::InvalidRequest {
                    message: e.to_string(),
                })?;

        let response = match client.serve(request).await {
            Ok(r) => r,
            Err(e) => {
                if retry.retry_transport && attempt + 1 < max_attempts {
                    tracing::warn!(attempt, "Anthropic transport error, retrying: {e}");
                    continue;
                }
                return Err(AbiError::Transport {
                    status: 0,
                    message: e.to_string(),
                });
            }
        };

        let status = response.status();
        if status != StatusCode::OK {
            let err_body = response
                .into_body()
                .collect()
                .await
                .map_err(|e| AbiError::Transport {
                    status: status.as_u16(),
                    message: e.to_string(),
                })?
                .to_bytes();
            let body_text = String::from_utf8_lossy(&err_body);

            // Retry on 429 / 5xx if configured
            let retryable = (status == StatusCode::TOO_MANY_REQUESTS && retry.retry_429)
                || (status.is_server_error() && retry.retry_5xx);
            if retryable && attempt + 1 < max_attempts {
                tracing::warn!(attempt, status = %status, "Anthropic retryable error, retrying");
                continue;
            }

            return Err(AbiError::Transport {
                status: status.as_u16(),
                message: body_text.to_string(),
            });
        }

        return process_sse_data_stream(response.into_body().into_data_stream(), idle_timeout, tx)
            .await;
    }

    Err(AbiError::Transport {
        status: 0,
        message: "all retry attempts exhausted".to_string(),
    })
}

async fn process_sse_data_stream<S, E>(
    mut data_stream: S,
    idle_timeout: Duration,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut tool_acc: Option<ToolUseAccumulator> = None;
    let mut text_acc: Option<TextAccumulator> = None;
    let mut buffer = String::new();

    use futures::StreamExt;
    loop {
        let chunk = match timeout(idle_timeout, data_stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(err))) => return Err(AbiError::Stream(err.to_string())),
            Ok(None) => {
                return Err(AbiError::Stream(
                    "stream closed before Anthropic completion".to_string(),
                ));
            }
            Err(_) => return Err(AbiError::Stream("idle timeout waiting for SSE".to_string())),
        };
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

            let events = parse_sse_event(&event_type, &json, &mut tool_acc, &mut text_acc)?;
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
}

// ── Model discovery ───────────────────────────────────────────────

/// Fetch model list from an Anthropic-compatible `/models` endpoint.
///
/// The response shape is `{ "data": [{ "id", "display_name", ... }] }`.
/// Works on api.anthropic.com (the original) and Z.ai (who at least
/// added their own models to the response). MiniMax and Kimi copied
/// the wire format but forgot to implement discovery, so they 404
/// here and get `ListModelsError::Unsupported`. You can clone the
/// protocol but apparently not the whole API surface.
async fn fetch_anthropic_models(
    url: &str,
    headers: &HeaderMap,
) -> Result<Vec<chaos_abi::AbiModelInfo>, chaos_abi::ListModelsError> {
    use rama::Service;
    use rama::http::Body;
    use rama::http::Request;
    use rama::http::StatusCode;
    use rama::http::body::util::BodyExt;
    use rama::http::client::EasyHttpWebClient;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct ModelsResponse {
        data: Vec<ModelEntry>,
    }

    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        max_input_tokens: Option<i64>,
        #[serde(default)]
        max_tokens: Option<i64>,
        #[serde(default)]
        capabilities: Option<Capabilities>,
    }

    #[derive(Deserialize)]
    struct Capabilities {
        #[serde(default)]
        thinking: Option<Supported>,
        #[serde(default)]
        image_input: Option<Supported>,
        #[serde(default)]
        structured_outputs: Option<Supported>,
        #[serde(default)]
        effort: Option<Supported>,
    }

    #[derive(Deserialize)]
    struct Supported {
        #[serde(default)]
        supported: bool,
    }

    let client = EasyHttpWebClient::default();
    let mut builder = Request::builder().method("GET").uri(url);
    // Copy auth and version headers, skip content-type/accept (not needed for GET)
    for (name, value) in headers.iter() {
        let dominated = name == http::header::CONTENT_TYPE || name == http::header::ACCEPT;
        if !dominated {
            builder = builder.header(name, value);
        }
    }
    let request = builder
        .body(Body::empty())
        .map_err(|e| chaos_abi::ListModelsError::Failed {
            message: e.to_string(),
        })?;

    let response = client
        .serve(request)
        .await
        .map_err(|e| chaos_abi::ListModelsError::Failed {
            message: format!("transport: {e}"),
        })?;

    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Err(chaos_abi::ListModelsError::Unsupported);
    }
    if status != StatusCode::OK {
        let body = response
            .into_body()
            .collect()
            .await
            .map(|b| String::from_utf8_lossy(&b.to_bytes()).to_string())
            .unwrap_or_default();
        return Err(chaos_abi::ListModelsError::Failed {
            message: format!("HTTP {status}: {body}"),
        });
    }

    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| chaos_abi::ListModelsError::Failed {
            message: e.to_string(),
        })?
        .to_bytes();

    let resp: ModelsResponse =
        serde_json::from_slice(&body).map_err(|e| chaos_abi::ListModelsError::Failed {
            message: format!("parse: {e}"),
        })?;

    let models = resp
        .data
        .into_iter()
        .map(|m| {
            let caps = m.capabilities.as_ref();
            chaos_abi::AbiModelInfo {
                id: m.id.clone(),
                display_name: m.display_name.unwrap_or(m.id),
                max_input_tokens: m.max_input_tokens,
                max_output_tokens: m.max_tokens,
                supports_thinking: caps
                    .and_then(|c| c.thinking.as_ref())
                    .is_some_and(|s| s.supported),
                supports_images: caps
                    .and_then(|c| c.image_input.as_ref())
                    .is_some_and(|s| s.supported),
                supports_structured_output: caps
                    .and_then(|c| c.structured_outputs.as_ref())
                    .is_some_and(|s| s.supported),
                supports_reasoning_effort: caps
                    .and_then(|c| c.effort.as_ref())
                    .is_some_and(|s| s.supported),
            }
        })
        .collect();

    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use http::header::AUTHORIZATION;
    use std::time::Duration;

    fn test_provider() -> Provider {
        use crate::provider::RetryConfig;

        Provider {
            name: "Anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            query_params: None,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: true,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_millis(10),
        }
    }

    #[test]
    fn build_headers_uses_x_api_key_for_api_key_auth() {
        let adapter = AnthropicAdapter::new(
            test_provider(),
            AnthropicAuth::ApiKey("sk-ant".to_string()),
            None,
        );

        let headers = adapter.build_headers().expect("headers should build");

        assert_eq!(
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("sk-ant")
        );
        assert!(headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn build_headers_uses_bearer_for_bearer_auth() {
        let adapter = AnthropicAdapter::new(
            test_provider(),
            AnthropicAuth::BearerToken("tok-ant".to_string()),
            None,
        );

        let headers = adapter.build_headers().expect("headers should build");

        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer tok-ant")
        );
        assert!(headers.get("x-api-key").is_none());
    }

    #[tokio::test]
    async fn process_sse_data_stream_times_out_when_idle() {
        let (tx, _rx) = mpsc::channel(4);
        let stream = stream::pending::<Result<Bytes, std::io::Error>>();

        let err = process_sse_data_stream(stream, Duration::from_millis(5), tx)
            .await
            .expect_err("idle stream should time out");

        assert!(
            matches!(err, AbiError::Stream(message) if message == "idle timeout waiting for SSE")
        );
    }
}
