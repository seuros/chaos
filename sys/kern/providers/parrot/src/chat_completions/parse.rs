use std::collections::BTreeMap;

use chaos_abi::AbiError;
use chaos_abi::ContentItem;
use chaos_abi::MessagePhase;
use chaos_abi::ResponseItem;
use chaos_abi::TokenUsage;
use chaos_abi::TurnEvent;
use serde_json::Value;

/// In-flight tool call accumulator for a single tool_calls index.
#[derive(Default)]
pub(crate) struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
    provider_metadata: Option<Value>,
}

/// In-flight text accumulator so we can finalize the assistant message.
#[derive(Default)]
pub(crate) struct TextAccumulator {
    text: String,
}

/// Parse a single `data: <json>` line from the Chat Completions SSE stream.
///
/// The schema is:
/// ```json
/// {"id":"chatcmpl-...","object":"chat.completion.chunk","model":"example-model",
///  "choices":[{"delta":{"role":"assistant","content":"...","tool_calls":[...]},
///              "finish_reason":null}],
///  "usage":null}
/// ```
/// The final chunk carries `"finish_reason": "stop"` or similar, and when
/// `stream_options.include_usage=true` the very last chunk has `"usage"`.
/// Build a `Completed` event from a chat-completions `usage` object,
/// mapping `prompt_tokens`/`completion_tokens` to the token-usage totals.
pub(super) fn completed_event_from_usage(response_id: &str, usage: &Value) -> TurnEvent {
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(Value::as_i64)
        .filter(|count| *count >= 0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(Value::as_i64)
        .filter(|count| *count >= 0);
    let token_usage = input_tokens
        .zip(output_tokens)
        .and_then(|(input_tokens, output_tokens)| {
            Some(TokenUsage {
                input_tokens,
                output_tokens,
                cached_input_tokens: usage
                    .pointer("/prompt_tokens_details/cached_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                reasoning_output_tokens: usage
                    .pointer("/completion_tokens_details/reasoning_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                total_tokens: match usage.get("total_tokens").and_then(Value::as_i64) {
                    Some(total) => total,
                    None => input_tokens.checked_add(output_tokens)?,
                },
                ..Default::default()
            })
        });
    TurnEvent::Completed {
        response_id: response_id.to_string(),
        token_usage,
    }
}

pub(crate) fn parse_chunk(
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
                // No phase from the wire: text beside tool calls is preamble.
                let phase = if tool_acc.is_empty() {
                    Some(MessagePhase::FinalAnswer)
                } else {
                    Some(MessagePhase::Commentary)
                };
                events.push(TurnEvent::OutputItemDone(ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText { text: acc.text }],
                    phase,
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
