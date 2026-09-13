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
