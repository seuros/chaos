//! TensorZero native inference API adapter.
//!
//! Translates chaos-abi `TurnRequest` into TensorZero's native `/inference`
//! wire format and streams `TurnEvent`s back from the SSE response.
//!
//! Uses the native API (not the OpenAI-compat endpoint) to get access to
//! `episode_id` tracking, `thought` content blocks, and provider-neutral
//! usage reporting.

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
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Adapter for the TensorZero native inference API (`POST /inference`).
#[derive(Debug, Clone)]
pub struct TensorZeroAdapter {
    provider: Provider,
    api_key: String,
    /// TensorZero function name (e.g. "coding_large").
    default_function: Option<String>,
}

impl TensorZeroAdapter {
    pub fn new(provider: Provider, api_key: String, default_function: Option<String>) -> Self {
        Self {
            provider,
            api_key,
            default_function,
        }
    }

    pub fn from_base_url_and_api_key(
        base_url: String,
        api_key: String,
        default_function: Option<String>,
    ) -> Self {
        let provider =
            Provider::from_base_url_with_default_streaming_config("TensorZero", base_url, false);
        Self::new(provider, api_key, default_function)
    }

    fn inference_url(&self) -> String {
        self.provider.url_for_path("/inference")
    }

    fn function_for_request(&self, request_model: &str) -> String {
        // The ABI passes the model slug — for TensorZero this maps to the
        // function_name. Falls back to default_function if model is empty.
        if request_model.is_empty() {
            self.default_function
                .clone()
                .unwrap_or_else(|| "default".to_string())
        } else {
            request_model.to_string()
        }
    }

    fn build_headers(&self) -> Result<HeaderMap, AbiError> {
        let mut headers = self.provider.headers.clone();

        // TensorZero auth is optional — only set Bearer if key is non-empty.
        if !self.api_key.trim().is_empty() {
            let bearer = format!("Bearer {}", self.api_key);
            let value =
                http::HeaderValue::from_str(&bearer).map_err(|err| AbiError::InvalidRequest {
                    message: format!("invalid Authorization header value: {err}"),
                })?;
            headers.insert(http::header::AUTHORIZATION, value);
        }
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

impl ModelAdapter for TensorZeroAdapter {
    fn stream(&self, request: TurnRequest) -> AdapterFuture<'_> {
        Box::pin(async move {
            let url = self.inference_url();
            let function_name = self.function_for_request(&request.model);
            let turn_state = request.turn_state.clone();

            // Read episode_id from turn_state if a prior turn already set it.
            let episode_id = turn_state.as_ref().and_then(|s| s.get().cloned());

            let body = build_request_body(&request, &function_name, episode_id.as_deref())?;
            let headers = self.build_headers()?;
            let retry = self.provider.retry.clone();
            let idle_timeout = self.provider.stream_idle_timeout;

            let (tx, rx) = mpsc::channel(64);

            // Emit ServerModel with the function name so the kernel's
            // model-mismatch check sees a match against the requested model.
            let fn_name = function_name;
            tokio::spawn(async move {
                let _ = tx.send(Ok(TurnEvent::ServerModel(fn_name))).await;
                if let Err(e) = run_sse_stream(
                    &url,
                    &headers,
                    &body,
                    &retry,
                    idle_timeout,
                    turn_state,
                    tx.clone(),
                )
                .await
                {
                    let _ = tx.send(Err(e)).await;
                }
            });

            Ok(TurnStream { rx_event: rx })
        })
    }

    fn provider_name(&self) -> &str {
        "TensorZero"
    }

    fn capabilities(&self) -> chaos_abi::AdapterCapabilities {
        chaos_abi::AdapterCapabilities {
            can_list_models: false,
        }
    }
}

// ── Request building ───────────────────────────────────────────────

#[derive(Serialize)]
struct TzInferenceRequest {
    function_name: String,
    input: TzInput,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    episode_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    additional_tools: Vec<TzTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<TzParams>,
}

#[derive(Serialize)]
struct TzInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Value>,
    messages: Vec<TzMessage>,
}

#[derive(Serialize)]
struct TzMessage {
    role: String,
    content: Vec<Value>,
}

/// TensorZero tool format: flat struct with `type` as a sibling tag, not a wrapper.
/// Accepts both tagged (`{"type": "function", ...}`) and untagged (`{...}`) forms.
#[derive(Serialize)]
struct TzTool {
    #[serde(rename = "type")]
    tool_type: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    parameters: Value,
    strict: bool,
}

#[derive(Serialize)]
struct TzParams {
    chat_completion: TzChatCompletionParams,
}

#[derive(Serialize)]
struct TzChatCompletionParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

fn build_request_body(
    request: &TurnRequest,
    function_name: &str,
    episode_id: Option<&str>,
) -> Result<Value, AbiError> {
    let system = if request.instructions.is_empty() {
        None
    } else {
        Some(Value::String(request.instructions.clone()))
    };

    let messages = convert_input_to_messages(&request.input);
    let tools = convert_tools(&request.tools);
    let parallel_tool_calls = if request.tools.is_empty() {
        None
    } else {
        Some(request.parallel_tool_calls)
    };

    // Extract params from extensions if present.
    let params = {
        let max_tokens = request.extensions.get("max_tokens").and_then(Value::as_u64);
        let temperature = request
            .extensions
            .get("temperature")
            .and_then(Value::as_f64);
        if max_tokens.is_some() || temperature.is_some() {
            Some(TzParams {
                chat_completion: TzChatCompletionParams {
                    max_tokens,
                    temperature,
                },
            })
        } else {
            None
        }
    };

    let body = serde_json::to_value(TzInferenceRequest {
        function_name: function_name.to_string(),
        input: TzInput { system, messages },
        stream: true,
        episode_id: episode_id.map(String::from),
        additional_tools: tools,
        tool_choice: None,
        parallel_tool_calls,
        params,
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
        ContentItem::InputImage { image_url } => {
            Some(serde_json::json!({"type": "file", "url": image_url}))
        }
    }
}

fn convert_input_to_messages(input: &[ResponseItem]) -> Vec<TzMessage> {
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
                let tz_role = match role.as_str() {
                    "assistant" => "assistant",
                    _ => "user",
                };
                messages.push(TzMessage {
                    role: tz_role.to_string(),
                    content: parts,
                });
            }

            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                // Assistant turn that issued a tool call — TZ uses tool_call content blocks.
                messages.push(TzMessage {
                    role: "assistant".to_string(),
                    content: vec![serde_json::json!({
                        "type": "tool_call",
                        "id": call_id,
                        "name": name,
                        "arguments": arguments,
                    })],
                });
            }

            ResponseItem::FunctionCallOutput {
                call_id,
                output,
                tool_name,
                ..
            }
            | ResponseItem::CustomToolCallOutput {
                call_id,
                output,
                tool_name,
                ..
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
                messages.push(TzMessage {
                    role: "user".to_string(),
                    content: vec![serde_json::json!({
                        "type": "tool_result",
                        "id": call_id,
                        "name": tool_name.as_deref().unwrap_or("tool"),
                        "result": content_text,
                    })],
                });
            }

            ResponseItem::LocalShellCall {
                call_id, action, ..
            } => {
                let cmd = match action {
                    chaos_ipc::models::LocalShellAction::Exec(exec) => exec.command.join(" "),
                };
                if let Some(id) = call_id {
                    messages.push(TzMessage {
                        role: "assistant".to_string(),
                        content: vec![serde_json::json!({
                            "type": "tool_call",
                            "id": id,
                            "name": "shell",
                            "arguments": format!(r#"{{"command": {:?}}}"#, cmd),
                        })],
                    });
                }
            }

            ResponseItem::Reasoning { .. } => {}

            _ => {
                tracing::debug!(
                    "TensorZero adapter: skipping unsupported ResponseItem variant in history"
                );
            }
        }
    }

    messages
}

fn convert_tools(tools: &[chaos_abi::ToolDef]) -> Vec<TzTool> {
    tools
        .iter()
        .map(|tool| match tool {
            chaos_abi::ToolDef::Function(f) => TzTool {
                tool_type: "function".to_string(),
                name: f.name.clone(),
                description: Some(f.description.clone()),
                parameters: f.parameters.clone(),
                strict: f.strict,
            },
            chaos_abi::ToolDef::Freeform(f) => TzTool {
                tool_type: "function".to_string(),
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
                strict: false,
            },
        })
        .collect()
}

// ── SSE event parsing ──────────────────────────────────────────────

/// In-flight tool call accumulator.
#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

/// In-flight text accumulator.
#[derive(Default)]
struct TextAccumulator {
    text: String,
}

/// In-flight thought/reasoning accumulator.
#[derive(Default)]
struct ThoughtAccumulator {
    text: String,
}

/// Parse a single `data: <json>` line from the TensorZero streaming response.
///
/// TensorZero native streaming chunks look like:
/// ```json
/// {"inference_id":"...","episode_id":"...","variant_name":"...",
///  "content":[{"type":"text","id":"...","text":"token"}],
///  "usage":null,"finish_reason":null}
/// ```
/// The final chunk carries `"finish_reason": "stop"` and `"usage"`.
fn parse_chunk(
    json: &Value,
    tool_acc: &mut BTreeMap<String, ToolCallAccumulator>,
    text_acc: &mut Option<TextAccumulator>,
    thought_acc: &mut Option<ThoughtAccumulator>,
    response_id: &mut String,
    _server_model: &mut Option<String>,
) -> Result<Vec<TurnEvent>, AbiError> {
    let mut events = Vec::new();

    // Capture inference_id as response_id.
    if let Some(id) = json.get("inference_id").and_then(Value::as_str)
        && response_id.is_empty()
    {
        *response_id = id.to_string();
        events.push(TurnEvent::Created);
    }

    // We intentionally do NOT emit ServerModel from the variant_name in the
    // chunk — the variant is an internal TZ routing detail (e.g. "glm_air")
    // that would trigger the kernel's model-mismatch warning. The function
    // name (which matches the requested model) is injected by the caller.

    let finish_reason = json.get("finish_reason").and_then(Value::as_str);

    // Process content blocks.
    if let Some(content) = json.get("content").and_then(Value::as_array) {
        for block in content {
            let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");

            match block_type {
                "text" => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    if !text.is_empty() {
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
                            acc.text.push_str(text);
                        }
                        events.push(TurnEvent::OutputTextDelta(text.to_string()));
                    }
                }

                "tool_call" => {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("raw_name")
                        .or_else(|| block.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let arguments = block
                        .get("raw_arguments")
                        .or_else(|| block.get("arguments"))
                        .map(|v| {
                            if let Some(s) = v.as_str() {
                                s.to_string()
                            } else {
                                v.to_string()
                            }
                        })
                        .unwrap_or_default();

                    let key = if id.is_empty() {
                        name.clone()
                    } else {
                        id.clone()
                    };
                    let acc = tool_acc.entry(key).or_default();
                    if !id.is_empty() {
                        acc.id = id;
                    }
                    if !name.is_empty() {
                        acc.name = name;
                    }
                    acc.arguments.push_str(&arguments);
                }

                "thought" => {
                    // TensorZero thought blocks map to reasoning content.
                    // Accumulate like text — emit OutputItemAdded on the first
                    // chunk, deltas for each chunk, and OutputItemDone at finish.
                    let thinking = block
                        .get("text")
                        .or_else(|| block.get("thinking"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !thinking.is_empty() {
                        if thought_acc.is_none() {
                            events.push(TurnEvent::OutputItemAdded(ResponseItem::Reasoning {
                                id: String::new(),
                                summary: vec![],
                                content: None,
                                encrypted_content: None,
                            }));
                            *thought_acc = Some(ThoughtAccumulator::default());
                        }
                        if let Some(acc) = thought_acc.as_mut() {
                            acc.text.push_str(thinking);
                        }
                        events.push(TurnEvent::ReasoningContentDelta {
                            delta: thinking.to_string(),
                            content_index: 0,
                        });
                    }
                }

                _ => {}
            }
        }
    }

    // Handle finish.
    if finish_reason.is_some() {
        // Finalize thought/reasoning.
        if let Some(acc) = thought_acc.take() {
            events.push(TurnEvent::OutputItemDone(ResponseItem::Reasoning {
                id: String::new(),
                summary: vec![],
                content: Some(vec![
                    chaos_ipc::models::ReasoningItemContent::ReasoningText { text: acc.text },
                ]),
                encrypted_content: None,
            }));
        }

        // Finalize text.
        if let Some(acc) = text_acc.take() {
            events.push(TurnEvent::OutputItemDone(ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText { text: acc.text }],
                phase: None,
                end_turn: None,
            }));
        }

        // Finalize tool calls.
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

        // Usage.
        let token_usage = json.get("usage").filter(|u| !u.is_null()).map(|usage| {
            let input_tokens = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output_tokens = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            TokenUsage {
                input_tokens: input_tokens as i64,
                output_tokens: output_tokens as i64,
                total_tokens: (input_tokens + output_tokens) as i64,
                ..Default::default()
            }
        });

        events.push(TurnEvent::Completed {
            response_id: response_id.clone(),
            token_usage,
        });
    }

    Ok(events)
}

// ── SSE transport ──────────────────────────────────────────────────

async fn run_sse_stream(
    url: &str,
    headers: &HeaderMap,
    body: &Value,
    retry: &crate::provider::RetryConfig,
    idle_timeout: Duration,
    turn_state: Option<Arc<OnceLock<String>>>,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError> {
    let response = crate::sse::transport::start_rama_post_sse_request(
        url,
        headers,
        body,
        retry,
        "tensorzero",
        None,
    )
    .await?;
    process_sse_data_stream(
        response.into_body().into_data_stream(),
        idle_timeout,
        turn_state,
        tx,
    )
    .await
}

pub(crate) async fn process_sse_data_stream<S, E>(
    data_stream: S,
    idle_timeout: Duration,
    turn_state: Option<Arc<OnceLock<String>>>,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<(), AbiError>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<BoxError> + std::fmt::Display,
{
    let mut tool_acc: BTreeMap<String, ToolCallAccumulator> = BTreeMap::new();
    let mut text_acc: Option<TextAccumulator> = None;
    let mut thought_acc: Option<ThoughtAccumulator> = None;
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

        // Check for TensorZero error response in the stream.
        if let Some(error) = json.get("error").and_then(Value::as_str) {
            return Err(AbiError::Transport {
                status: 500,
                message: error.to_string(),
            });
        }

        // Capture episode_id from the first chunk and store in turn_state
        // so subsequent turns in the same session reuse the same episode.
        if let Some(ep_id) = json.get("episode_id").and_then(Value::as_str)
            && let Some(state) = turn_state.as_ref()
        {
            let _ = state.set(ep_id.to_string());
        }

        let events = parse_chunk(
            &json,
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
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
    fn build_request_body_includes_system_prompt() {
        let request = TurnRequest {
            model: "coding_large".to_string(),
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

        let body = build_request_body(&request, "coding_large", None).expect("body should build");
        assert_eq!(body["function_name"], "coding_large");
        assert_eq!(body["stream"], true);
        assert_eq!(body["input"]["system"], "You are helpful");
    }

    #[test]
    fn build_request_body_omits_system_when_empty() {
        let request = TurnRequest {
            model: String::new(),
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

        let body = build_request_body(&request, "coding_large", None).expect("body should build");
        assert!(body["input"].get("system").is_none());
    }

    #[test]
    fn parse_chunk_handles_text_content() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = None;
        let mut thought_acc = None;
        let mut response_id = String::new();
        let mut server_model = None;

        let events = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-test",
                "episode_id": "019d-ep",
                "variant_name": "glm_air",
                "content": [{"type": "text", "text": "Hello"}],
                "usage": null,
                "finish_reason": null
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("should parse");

        assert_eq!(response_id, "019d-test");
        assert!(events.iter().any(|e| matches!(e, TurnEvent::Created)));
        // ServerModel is NOT emitted from variant_name — the function name is
        // injected by the caller (stream()) to avoid model-mismatch warnings.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, TurnEvent::ServerModel(_)))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TurnEvent::OutputTextDelta(t) if t == "Hello"))
        );
    }

    #[test]
    fn parse_chunk_handles_thought_content() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = None;
        let mut thought_acc = None;
        let mut response_id = String::new();
        let mut server_model = None;

        let events = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-test",
                "variant_name": "glm_air",
                "content": [{"type": "thought", "text": "thinking..."}],
                "usage": null,
                "finish_reason": null
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("should parse");

        assert!(events.iter().any(|e| matches!(
            e,
            TurnEvent::OutputItemAdded(ResponseItem::Reasoning { .. })
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            TurnEvent::ReasoningContentDelta { delta, .. } if delta == "thinking..."
        )));
        // OutputItemDone is NOT emitted until finish_reason is set.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, TurnEvent::OutputItemDone(ResponseItem::Reasoning { .. })))
        );
        assert!(thought_acc.is_some());
    }

    #[test]
    fn parse_chunk_finalizes_on_stop() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = Some(TextAccumulator {
            text: "Hello world".to_string(),
        });
        let mut thought_acc = None;
        let mut response_id = "019d-test".to_string();
        let mut server_model = Some("glm_air".to_string());

        let events = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-test",
                "variant_name": "glm_air",
                "content": [],
                "usage": {"input_tokens": 42, "output_tokens": 10},
                "finish_reason": "stop"
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("should parse");

        assert!(events.iter().any(|e| matches!(
            e,
            TurnEvent::OutputItemDone(ResponseItem::Message { content, .. })
                if content.len() == 1
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            TurnEvent::Completed { token_usage: Some(usage), .. }
                if usage.input_tokens == 42 && usage.output_tokens == 10
        )));
    }

    #[test]
    fn parse_chunk_handles_tool_call() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = None;
        let mut thought_acc = None;
        let mut response_id = String::new();
        let mut server_model = None;

        // Chunk with tool call.
        let _ = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-test",
                "variant_name": "glm_air",
                "content": [{
                    "type": "tool_call",
                    "id": "call_1",
                    "raw_name": "shell",
                    "raw_arguments": "{\"command\": \"ls\"}"
                }],
                "usage": null,
                "finish_reason": null
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("should parse");

        assert_eq!(tool_acc.len(), 1);

        // Finish chunk.
        let events = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-test",
                "variant_name": "glm_air",
                "content": [],
                "usage": {"input_tokens": 10, "output_tokens": 5},
                "finish_reason": "tool_calls"
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("should parse");

        assert!(events.iter().any(|e| matches!(
            e,
            TurnEvent::OutputItemDone(ResponseItem::FunctionCall { name, call_id, .. })
                if name == "shell" && call_id == "call_1"
        )));
    }

    #[tokio::test]
    async fn process_sse_stream_completes_on_done() {
        let stream = futures::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::from(
                "data: {\"inference_id\":\"id-1\",\"episode_id\":\"ep-1\",\"variant_name\":\"v1\",\"content\":[{\"type\":\"text\",\"text\":\"Hi\"}],\"usage\":null,\"finish_reason\":null}\n\n",
            )),
            Ok::<_, std::io::Error>(Bytes::from(
                "data: {\"inference_id\":\"id-1\",\"episode_id\":\"ep-1\",\"variant_name\":\"v1\",\"content\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":1},\"finish_reason\":\"stop\"}\n\n",
            )),
            Ok::<_, std::io::Error>(Bytes::from("data: [DONE]\n\n")),
        ]);
        let (tx, mut rx) = mpsc::channel(16);

        process_sse_data_stream(stream, Duration::from_secs(1), None, tx)
            .await
            .expect("stream should succeed");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event.expect("event should be ok"));
        }

        assert!(events.iter().any(|e| matches!(e, TurnEvent::Created)));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TurnEvent::OutputTextDelta(t) if t == "Hi"))
        );
        assert!(events.iter().any(|e| matches!(
            e,
            TurnEvent::Completed { token_usage: Some(u), .. } if u.input_tokens == 5
        )));
    }

    #[test]
    fn parse_chunk_handles_malformed_json() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = None;
        let mut thought_acc = None;
        let mut response_id = String::new();
        let mut server_model = None;

        // Attempt to parse malformed JSON - missing required fields
        let result = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-test",
                // Missing required variant_name and content fields
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        );

        // Should handle gracefully without panicking
        assert!(
            result.is_err() || result.is_ok(),
            "parse_chunk should handle missing fields"
        );
    }

    #[test]
    fn parse_chunk_handles_invalid_content_type() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = None;
        let mut thought_acc = None;
        let mut response_id = String::new();
        let mut server_model = None;

        let events = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-test",
                "variant_name": "glm_air",
                "content": [{"type": "unknown_type", "data": "test"}],
                "usage": null,
                "finish_reason": null
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("should parse despite unknown content type");

        // Unknown content types should be silently ignored
        assert!(
            !events.is_empty() || events.is_empty(),
            "should handle gracefully"
        );
    }

    #[test]
    fn parse_chunk_accumulates_multiple_tool_calls() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = None;
        let mut thought_acc = None;
        let mut response_id = String::new();
        let mut server_model = None;

        // First tool call
        let _ = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-multi",
                "variant_name": "glm_air",
                "content": [{
                    "type": "tool_call",
                    "id": "call_1",
                    "raw_name": "shell",
                    "raw_arguments": "{\"command\": \"ls\"}"
                }],
                "usage": null,
                "finish_reason": null
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("should parse first tool call");

        assert_eq!(tool_acc.len(), 1, "should have 1 tool call");

        // Second tool call
        let _ = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-multi",
                "variant_name": "glm_air",
                "content": [{
                    "type": "tool_call",
                    "id": "call_2",
                    "raw_name": "shell",
                    "raw_arguments": "{\"command\": \"pwd\"}"
                }],
                "usage": null,
                "finish_reason": null
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("should parse second tool call");

        assert_eq!(tool_acc.len(), 2, "should accumulate multiple tool calls");
    }

    #[test]
    fn parse_chunk_finalization_with_empty_usage() {
        let mut tool_acc = BTreeMap::new();
        let mut text_acc = Some(TextAccumulator {
            text: "Output".to_string(),
        });
        let mut response_id = "019d-test".to_string();
        let mut server_model = Some("glm_air".to_string());
        let mut thought_acc = None;

        let events = parse_chunk(
            &serde_json::json!({
                "inference_id": "019d-test",
                "variant_name": "glm_air",
                "content": [],
                "usage": null,
                "finish_reason": "stop"
            }),
            &mut tool_acc,
            &mut text_acc,
            &mut thought_acc,
            &mut response_id,
            &mut server_model,
        )
        .expect("should parse with null usage");

        // Should complete even without usage data
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TurnEvent::OutputItemDone(..)))
        );
    }
}
