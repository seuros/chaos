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
        let adapter =
            ChatCompletionsAdapter::from_base_url_and_api_key(server.uri(), "secret".into(), None);
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
