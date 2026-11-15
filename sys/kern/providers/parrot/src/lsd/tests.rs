use super::request::build_request;
use super::request::fingerprint;
use super::stream::ChunkFolder;
use super::*;
use chaos_abi::ContentItem;
use chaos_abi::ReasoningConfig;
use chaos_abi::ResponseItem;
use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::models::FunctionCallOutputBody;
use chaos_ipc::models::FunctionCallOutputPayload;
use chaos_ipc::models::ReasoningItemContent;
use chaos_ipc::openai_models::ReasoningEffort;
use lsd_protocol::Content;
use lsd_protocol::Effort;
use lsd_protocol::File;
use lsd_protocol::ThinkingDisplay;
use lsd_protocol::fixtures::CHUNK_STREAMS;
use pretty_assertions::assert_eq;
use rama::futures;

fn turn(model: &str) -> TurnRequest {
    TurnRequest {
        continuation: None,
        model: model.into(),
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

fn user(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputText { text: text.into() }],
        end_turn: None,
        phase: None,
    }
}

fn assistant(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".into(),
        content: vec![ContentItem::OutputText { text: text.into() }],
        end_turn: None,
        phase: None,
    }
}

fn call(id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: "shell".into(),
        namespace: None,
        arguments: r#"{"command":"ls"}"#.into(),
        call_id: id.into(),
        provider_metadata: None,
    }
}

fn output(id: &str, success: Option<bool>) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: id.into(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("done".into()),
            success,
        },
        tool_name: Some("shell".into()),
    }
}

fn body(request: &TurnRequest) -> Value {
    serde_json::to_value(build_request(request, &request.model).unwrap()).unwrap()
}

#[test]
fn bare_targets_are_functions_and_qualified_targets_are_models() {
    assert_eq!(
        body(&turn("coding_large")),
        serde_json::json!({
            "function_name": "coding_large",
            "input": {"messages": []},
            "stream": true,
        })
    );
    assert_eq!(
        body(&turn("anthropic::claude-opus-5-5"))["model_name"],
        "anthropic::claude-opus-5-5"
    );
}

#[test]
fn output_cap_wins_over_the_legacy_extension_and_invalid_caps_fail() {
    let mut request = turn("coding_large");
    request
        .extensions
        .insert("max_tokens".into(), serde_json::json!(1234));
    request
        .extensions
        .insert("temperature".into(), serde_json::json!(0.25));
    assert_eq!(
        body(&request)["params"],
        serde_json::json!({"chat_completion": {"max_tokens": 1234, "temperature": 0.25}})
    );
    request
        .extensions
        .insert("max_output_tokens".into(), serde_json::json!(37));
    assert_eq!(
        body(&request)["params"]["chat_completion"]["max_tokens"],
        37
    );
    for invalid in [
        serde_json::json!(0),
        serde_json::json!("37"),
        serde_json::json!(-1),
    ] {
        request
            .extensions
            .insert("max_output_tokens".into(), invalid);
        std::assert_matches!(
            build_request(&request, "coding_large"),
            Err(AbiError::InvalidRequest { .. })
        );
    }
}

#[test]
fn reasoning_maps_to_effort_and_display() {
    let mut request = turn("coding_large");
    request.instructions = "Be terse.".into();
    request.reasoning = Some(ReasoningConfig {
        effort: Some(ReasoningEffort::XHigh),
        summary: Some(ReasoningSummary::Detailed),
    });
    let wire = build_request(&request, "coding_large").unwrap();
    assert_eq!(
        wire.params.chat_completion.reasoning_effort,
        Some(Effort::Xhigh)
    );
    assert_eq!(
        wire.params.chat_completion.thinking_display,
        Some(ThinkingDisplay::Summarized)
    );
    assert_eq!(body(&request)["input"]["system"], "Be terse.");
}

#[test]
fn a_reply_travels_as_one_assistant_message_with_its_signed_thought() {
    let mut request = turn("coding_large");
    request.input = vec![
        ResponseItem::Message {
            id: None,
            role: "developer".into(),
            content: vec![ContentItem::InputText {
                text: "Sandbox is read-only.".into(),
            }],
            end_turn: None,
            phase: None,
        },
        user("list files"),
        ResponseItem::Reasoning {
            id: String::new(),
            summary: vec![],
            content: Some(vec![ReasoningItemContent::ReasoningText {
                text: "check".into(),
            }]),
            encrypted_content: Some("sig".into()),
        },
        assistant("Listing."),
        call("c1"),
        call("c2"),
        output("c1", Some(true)),
        output("c2", Some(false)),
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputImage {
                image_url: "data:image/png;base64,AAAA".into(),
            }],
            end_turn: None,
            phase: None,
        },
    ];
    let messages = build_request(&request, "coding_large")
        .unwrap()
        .input
        .messages;
    let roles: Vec<Role> = messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [Role::System, Role::User, Role::Assistant, Role::User]
    );

    let reply = &messages[2].content;
    assert_eq!(reply.len(), 4);
    std::assert_matches!(&reply[0], Content::Thought(t) if t.signature.as_deref() == Some("sig"));

    let results = &messages[3].content;
    std::assert_matches!(&results[0], Content::ToolResult(r) if !r.is_error);
    std::assert_matches!(&results[1], Content::ToolResult(r) if r.is_error && r.name == "shell");
    std::assert_matches!(
        &results[2],
        Content::File(File::Base64 { mime_type, data, .. })
            if mime_type == "image/png" && data == "AAAA"
    );
}

fn full(input: Vec<ResponseItem>) -> InferenceRequest {
    let mut request = turn("coding_large");
    request.input = input;
    build_request(&request, "coding_large").unwrap()
}

fn stored(after: &InferenceRequest, trip: &str) -> Continuation {
    let prefix_len = after.input.messages.len();
    Continuation {
        token: trip.into(),
        prefix_len,
        fingerprint: fingerprint(after, prefix_len),
    }
}

const TRIP: &str = "01938d2a-7c4e-7a10-9f5e-1b2c3d4e5f60";

#[test]
fn a_turn_that_extends_the_trip_sends_only_what_follows_the_stored_reply() {
    let first = full(vec![user("list files")]);
    let next = full(vec![
        user("list files"),
        assistant("Listing."),
        call("c1"),
        output("c1", None),
    ]);
    let body = plan(&next, Some(&stored(&first, TRIP)));
    assert!(body.continue_trip);
    assert_eq!(body.trip_id.map(|id| id.to_string()).as_deref(), Some(TRIP));
    assert_eq!(body.input.messages.len(), 1);
    assert_eq!(body.input.messages[0].role, Role::User);
}

#[test]
fn a_rewritten_history_replays_everything_under_the_same_trip() {
    let first = full(vec![user("list files")]);
    let compacted = full(vec![
        user("summary of earlier work"),
        assistant("ok"),
        user("go on"),
    ]);
    let body = plan(&compacted, Some(&stored(&first, TRIP)));
    assert!(!body.continue_trip);
    assert_eq!(body.input.messages.len(), 3);
    assert_eq!(body.trip_id.map(|id| id.to_string()).as_deref(), Some(TRIP));
}

#[test]
fn nothing_new_after_the_reply_is_sent_in_full() {
    let first = full(vec![user("hi")]);
    let retry = full(vec![user("hi"), assistant("hello")]);
    assert!(!plan(&retry, Some(&stored(&first, TRIP))).continue_trip);
    assert!(!plan(&first, None).continue_trip);
}

#[test]
fn trip_loss_is_recognised_only_from_its_error_code() {
    let lost = AbiError::Transport {
        status: 404,
        message: r#"{"error":"gone","error_code":"trip_not_found"}"#.into(),
    };
    assert!(is_trip_not_found(&protocol_error(lost)));
    let unknown = AbiError::Transport {
        status: 404,
        message: r#"{"error":"no such function","error_code":"unknown_target"}"#.into(),
    };
    std::assert_matches!(protocol_error(unknown), AbiError::InvalidRequest { .. });
    let limited = AbiError::Transport {
        status: 429,
        message: r#"{"error":"slow down","error_code":"rate_limited","retry_after_ms":1500}"#
            .into(),
    };
    std::assert_matches!(
        protocol_error(limited),
        AbiError::Retryable { delay: Some(d), .. } if d == Duration::from_millis(1500)
    );
}

fn sse(lines: &str) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
    let mut frames: Vec<Result<Bytes, std::io::Error>> = lines
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Ok(Bytes::from(format!("data: {line}\n\n"))))
        .collect();
    frames.push(Ok(Bytes::from_static(b"data: [DONE]\n\n")));
    futures::stream::iter(frames)
}

async fn drain(lines: &str) -> (Result<Option<uuid::Uuid>, AbiError>, Vec<TurnEvent>) {
    let (tx, mut rx) = mpsc::channel(64);
    let result = process_sse_data_stream(sse(lines), Duration::from_secs(1), tx).await;
    let mut events = Vec::new();
    while let Ok(Ok(event)) = rx.try_recv() {
        events.push(event);
    }
    (result, events)
}

#[tokio::test]
async fn the_golden_stream_folds_into_a_signed_reasoning_text_and_one_tool_call() {
    let (_, stream) = CHUNK_STREAMS[0];
    let (result, events) = drain(stream).await;
    assert_eq!(
        result.unwrap().map(|id| id.to_string()).as_deref(),
        Some(TRIP)
    );

    let done: Vec<&ResponseItem> = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::OutputItemDone(item) => Some(item),
            _ => None,
        })
        .collect();
    std::assert_matches!(
        done[0],
        ResponseItem::Reasoning { encrypted_content: Some(sig), .. } if sig == "sig-xyz"
    );
    std::assert_matches!(done[1], ResponseItem::Message { .. });
    std::assert_matches!(
        done[2],
        ResponseItem::FunctionCall { name, arguments, call_id, .. }
            if name == "shell" && arguments == r#"{"command":"pwd"}"# && call_id == "call_3"
    );
    std::assert_matches!(
        events.last(),
        Some(TurnEvent::Completed { token_usage: Some(usage), .. })
            if usage.input_tokens == 1200 && usage.cache_read_input_tokens == 30000
    );
}

#[tokio::test]
async fn a_refusal_ends_the_turn_as_a_typed_error() {
    let (_, stream) = CHUNK_STREAMS[1];
    let (result, events) = drain(stream).await;
    std::assert_matches!(
        result,
        Err(AbiError::Refusal { category: Some(c), .. }) if c == "cyber"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, TurnEvent::Completed { .. }))
    );
}

#[tokio::test]
async fn mid_stream_errors_keep_a_retryable_status_and_store_nothing() {
    let (result, events) = drain(r#"{"error":"inference failed","error_code":"other"}"#).await;
    std::assert_matches!(
        result,
        Err(AbiError::Transport { status: 500, message }) if message == "inference failed"
    );
    assert!(events.is_empty());
}

#[tokio::test]
async fn an_uncoded_mid_stream_error_keeps_its_text() {
    let (result, _) = drain(r#"{"error":"upstream reset"}"#).await;
    std::assert_matches!(
        result,
        Err(AbiError::Transport { status: 500, message }) if message == "upstream reset"
    );
}

#[test]
fn partial_usage_is_unknown_rather_than_zero() {
    let mut folder = ChunkFolder::default();
    let chunk: InferenceResponseChunk = serde_json::from_value(serde_json::json!({
        "inference_id": "01938d2a-8000-7a10-9f5e-1b2c3d4e5f61",
        "trip_id": TRIP,
        "variant_name": "v",
        "content": [],
        "usage": {"input_tokens": 5, "output_tokens": null},
        "finish_reason": "stop"
    }))
    .unwrap();
    let mut response_id = String::new();
    let (events, finished) = folder.fold(chunk, &mut response_id).unwrap();
    assert!(finished.is_some());
    std::assert_matches!(
        events.last(),
        Some(TurnEvent::Completed {
            token_usage: None,
            ..
        })
    );
}
