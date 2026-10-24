use super::*;

fn token_limit_request() -> TurnRequest {
    TurnRequest {
        model: "coding_large".into(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    }
}

#[test]
fn output_cap_is_opt_in_and_preserves_legacy_lsd_params() {
    let mut request = token_limit_request();
    let baseline = build_request_body(&request, "coding_large", Some("episode")).unwrap();
    assert_eq!(
        baseline,
        serde_json::json!({
            "function_name": "coding_large",
            "input": {"messages": []},
            "stream": true,
            "episode_id": "episode"
        })
    );
    for cap in [1, 37, u32::MAX] {
        request
            .extensions
            .insert("max_output_tokens".into(), serde_json::json!(cap));
        let mut body = build_request_body(&request, "coding_large", Some("episode")).unwrap();
        assert_eq!(body["params"]["chat_completion"]["max_tokens"], cap);
        assert!(body.get("max_output_tokens").is_none());
        body.as_object_mut().unwrap().remove("params");
        assert_eq!(
            serde_json::to_vec(&body).unwrap(),
            serde_json::to_vec(&baseline).unwrap()
        );
    }
    request.extensions.remove("max_output_tokens");
    request
        .extensions
        .insert("max_tokens".into(), serde_json::json!(1234));
    request
        .extensions
        .insert("temperature".into(), serde_json::json!(0.25));
    let legacy = build_request_body(&request, "coding_large", Some("episode")).unwrap();
    assert_eq!(
        legacy["params"],
        serde_json::json!({"chat_completion": {"max_tokens": 1234, "temperature": 0.25}})
    );
    request
        .extensions
        .insert("max_output_tokens".into(), serde_json::json!(37));
    let mut capped = build_request_body(&request, "coding_large", Some("episode")).unwrap();
    assert_eq!(capped["params"]["chat_completion"]["max_tokens"], 37);
    capped["params"]["chat_completion"]["max_tokens"] = serde_json::json!(1234);
    assert_eq!(capped, legacy);
}

#[test]
fn invalid_output_cap_cannot_fall_back_to_a_legacy_lsd_limit() {
    let mut request = token_limit_request();
    request
        .extensions
        .insert("max_tokens".into(), serde_json::json!(1234));
    for invalid in [
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("37"),
        serde_json::json!(null),
        serde_json::json!(u64::from(u32::MAX) + 1),
    ] {
        request
            .extensions
            .insert("max_output_tokens".into(), invalid);
        std::assert_matches!(
            build_request_body(&request, "coding_large", None),
            Err(AbiError::InvalidRequest { .. })
        );
    }
}

#[test]
fn build_request_body_includes_only_nonempty_system_prompts() {
    for instructions in ["You are helpful", ""] {
        let mut request = token_limit_request();
        request.instructions = instructions.to_string();
        let body = build_request_body(&request, "coding_large", None).expect("body should build");
        assert_eq!(body["function_name"], "coding_large");
        assert_eq!(body["stream"], true);
        assert_eq!(
            body["input"].get("system").and_then(Value::as_str),
            (!instructions.is_empty()).then_some(instructions),
        );
    }
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
        Ok::<_, std::io::Error>(Bytes::from(concat!(
            "data: {\"inference_id\":\"id-1\",\"episode_id\":\"ep-1\",",
            "\"variant_name\":\"v1\",\"content\":[{\"type\":\"text\",\"text\":\"Hi\"}],",
            "\"usage\":null,\"finish_reason\":null}\n\n",
        ))),
        Ok::<_, std::io::Error>(Bytes::from(concat!(
            "data: {\"inference_id\":\"id-1\",\"episode_id\":\"ep-1\",",
            "\"variant_name\":\"v1\",\"content\":[],",
            "\"usage\":{\"input_tokens\":5,\"output_tokens\":1},",
            "\"finish_reason\":\"stop\"}\n\n",
        ))),
        Ok::<_, std::io::Error>(Bytes::from("data: [DONE]\n\n")),
    ]);
    let (tx, mut rx) = mpsc::channel(16);
    let turn_state = Arc::new(OnceLock::new());

    process_sse_data_stream(stream, Duration::from_secs(1), Some(turn_state.clone()), tx)
        .await
        .expect("stream should succeed");
    assert_eq!(turn_state.get().map(String::as_str), Some("ep-1"));

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

#[tokio::test]
async fn process_sse_stream_preserves_provider_errors() {
    let stream = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(
        b"data: {\"error\":\"inference failed\",\"episode_id\":\"not-captured\"}\n\n",
    ))]);
    let (tx, mut rx) = mpsc::channel(8);
    let turn_state = Arc::new(OnceLock::new());
    let result =
        process_sse_data_stream(stream, Duration::from_secs(1), Some(turn_state.clone()), tx).await;
    std::assert_matches!(
        result,
        Err(AbiError::Transport { status: 500, message }) if message == "inference failed"
    );
    assert!(
        turn_state.get().is_none(),
        "error frames must not update episode state"
    );
    assert!(
        rx.try_recv().is_err(),
        "provider errors must not emit completion"
    );
}

#[test]
fn parse_chunk_ignores_missing_or_unknown_content_without_fabricating_output() {
    for chunk in [
        serde_json::json!({"inference_id": "019d-test"}),
        serde_json::json!({
            "inference_id": "019d-test",
            "variant_name": "glm_air",
            "content": [{"type": "unknown_type", "data": "test"}],
            "usage": null,
            "finish_reason": null
        }),
    ] {
        let mut tools = BTreeMap::new();
        let mut text = None;
        let mut thoughts = None;
        let mut response_id = String::new();
        let mut server_model = None;
        let events = parse_chunk(
            &chunk,
            &mut tools,
            &mut text,
            &mut thoughts,
            &mut response_id,
            &mut server_model,
        )
        .expect("unsupported content should be ignored");
        std::assert_matches!(events.as_slice(), [TurnEvent::Created]);
        assert_eq!(response_id, "019d-test");
        assert!(tools.is_empty());
        assert!(text.is_none());
        assert!(thoughts.is_none());
        assert!(server_model.is_none());
    }
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

#[test]
fn partial_usage_is_not_reported_as_known_zero() {
    for usage in [
        serde_json::json!({}),
        serde_json::json!({"input_tokens": 12}),
        serde_json::json!({"output_tokens": 0}),
        serde_json::json!({"input_tokens": -1, "output_tokens": 0}),
    ] {
        let mut tools = BTreeMap::new();
        let mut text = None;
        let mut thoughts = None;
        let mut id = String::new();
        let mut model = None;
        let events = parse_chunk(&serde_json::json!({
            "inference_id": "partial-usage", "content": [], "finish_reason": "stop", "usage": usage,
        }), &mut tools, &mut text, &mut thoughts, &mut id, &mut model).unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            TurnEvent::Completed {
                token_usage: None,
                ..
            }
        )));
    }
}
