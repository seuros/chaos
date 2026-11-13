use super::super::*;
use chaos_abi::ContentItem;
use chaos_abi::ResponseItem;
use chaos_abi::TurnEvent;
use chaos_test_fixtures::TEST_MODEL;
use serde_json::Value;
use std::collections::BTreeMap;

use super::fixtures::completion_fixture;

#[test]
fn complete_json_preserves_lifecycle_and_usage() {
    let events = parse_completion(&completion_fixture()).unwrap();
    std::assert_matches!(events.as_slice(), [
            TurnEvent::Created,
            TurnEvent::ServerModel(model),
            TurnEvent::OutputItemAdded(ResponseItem::Message { .. }),
            TurnEvent::OutputTextDelta(text),
            TurnEvent::OutputItemDone(ResponseItem::Message { content, .. }),
            TurnEvent::Completed { response_id, token_usage: Some(usage) },
        ] if model == "server-model"
            && text == "{\"ok\":true}"
            && matches!(
                content.as_slice(),
                [ContentItem::OutputText { text }] if text == "{\"ok\":true}"
            )
            && response_id == "chatcmpl-test"
            && usage.input_tokens == 10
            && usage.output_tokens == 5
            && usage.total_tokens == 17);
}

#[test]
fn complete_json_preserves_multiple_tool_calls_without_indexes() {
    let mut json = completion_fixture();
    json["choices"][0] = serde_json::json!({
        "message": {
            "content": null,
            "tool_calls": [
                {"id": "a", "type": "function",
                 "function": {"name": "first", "arguments": "{\"x\":1}"},
                 "extra_content": {"google": {"thought_signature": "signature"}}},
                {"id": "b", "type": "function", "function": {"name": "second", "arguments": "{}"}}
            ]
        },
        "finish_reason": "tool_calls"
    });
    json["usage"] = Value::Null;
    let events = parse_completion(&json).unwrap();
    std::assert_matches!(events.as_slice(), [
            TurnEvent::Created,
            TurnEvent::ServerModel(_),
            TurnEvent::OutputItemDone(ResponseItem::FunctionCall {
                call_id: first_id,
                name: first_name,
                arguments,
                provider_metadata: Some(metadata),
                ..
            }),
            TurnEvent::OutputItemDone(ResponseItem::FunctionCall {
                call_id: second_id,
                name: second_name,
                ..
            }),
            TurnEvent::Completed { token_usage: None, .. },
        ] if first_id == "a" && first_name == "first" && arguments == "{\"x\":1}"
            && metadata["google"]["thought_signature"] == "signature"
            && second_id == "b" && second_name == "second");
}

#[test]
fn complete_json_handles_absent_usage_and_missing_total() {
    let mut json = completion_fixture();
    json["usage"]
        .as_object_mut()
        .unwrap()
        .remove("total_tokens");
    std::assert_matches!(
        parse_completion(&json).unwrap().last(),
        Some(TurnEvent::Completed { token_usage: Some(usage), .. })
            if usage.total_tokens == 15
    );
    json.as_object_mut().unwrap().remove("usage");
    std::assert_matches!(
        parse_completion(&json).unwrap().last(),
        Some(TurnEvent::Completed {
            token_usage: None,
            ..
        })
    );
}

#[test]
fn partial_usage_is_not_a_zero_receipt_and_cache_reasoning_categories_survive() {
    let mut json = completion_fixture();
    json["usage"] = serde_json::json!({
        "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15,
        "prompt_tokens_details": {"cached_tokens": 7},
        "completion_tokens_details": {"reasoning_tokens": 2},
    });
    std::assert_matches!(parse_completion(&json).unwrap().last(),
        Some(TurnEvent::Completed { token_usage: Some(usage), .. })
            if usage.cache_read_input_tokens == 7 && usage.reasoning_output_tokens == 2);
    for usage in [
        serde_json::json!({}),
        serde_json::json!({"prompt_tokens": 0}),
        serde_json::json!({"completion_tokens": 5}),
        serde_json::json!({"prompt_tokens": -1, "completion_tokens": 5}),
    ] {
        json["usage"] = usage;
        std::assert_matches!(
            parse_completion(&json).unwrap().last(),
            Some(TurnEvent::Completed {
                token_usage: None,
                ..
            })
        );
    }
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
    std::assert_matches!(
        &finish_events[0],
        TurnEvent::OutputItemDone(ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        }) if name == "first" && arguments == "{\"a\":1}" && call_id == "call_1"
    );
    std::assert_matches!(
        &finish_events[1],
        TurnEvent::OutputItemDone(ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        }) if name == "second" && arguments == "{\"b\":2}" && call_id == "call_2"
    );
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

    let body =
        build_request_body(&request, "gemini-2.5-pro", true).expect("follow-up body should build");

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
