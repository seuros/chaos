use super::*;
use assert_matches::assert_matches;
use bytes::Bytes;
use chaos_abi::ResponseItem;
use chaos_client::StreamResponse;
use chaos_ipc::account::PlanType;
use chaos_ipc::models::MessagePhase;
use futures::stream;
use pretty_assertions::assert_eq;
use rama::http::HeaderMap;
use rama::http::HeaderValue;
use rama::http::StatusCode;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_test::io::Builder as IoBuilder;

async fn collect_events(chunks: &[&[u8]]) -> Vec<Result<ResponseEvent, ApiError>> {
    let mut builder = IoBuilder::new();
    for chunk in chunks {
        builder.read(chunk);
    }

    let reader = builder.build();
    let stream = ReaderStream::new(reader).map_err(|err| TransportError::Network(err.to_string()));
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(16);
    tokio::spawn(process_sse(Box::pin(stream), tx, idle_timeout(), None));

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    events
}

async fn run_sse(events: Vec<serde_json::Value>) -> Vec<ResponseEvent> {
    run_sse_with_options(events, false).await
}

async fn run_sse_with_options(
    events: Vec<serde_json::Value>,
    use_openai_codex_rate_limits: bool,
) -> Vec<ResponseEvent> {
    let mut body = String::new();
    for e in events {
        let kind = e
            .get("type")
            .and_then(|v| v.as_str())
            .expect("fixture event missing type");
        if e.as_object().map(|o| o.len() == 1).unwrap_or(false) {
            body.push_str(&format!("event: {kind}\n\n"));
        } else {
            body.push_str(&format!("event: {kind}\ndata: {e}\n\n"));
        }
    }

    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    let stream = ReaderStream::new(std::io::Cursor::new(body))
        .map_err(|err| TransportError::Network(err.to_string()));
    tokio::spawn(process_sse_with_options(
        Box::pin(stream),
        tx,
        idle_timeout(),
        None,
        use_openai_codex_rate_limits,
    ));

    let mut out = Vec::new();
    while let Some(ev) = rx.recv().await {
        out.push(ev.expect("channel closed"));
    }
    out
}

fn idle_timeout() -> Duration {
    Duration::from_millis(1000)
}

#[tokio::test]
async fn parses_items_and_completed() {
    let item1 = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Hello"}],
            "phase": "commentary"
        }
    })
    .to_string();

    let item2 = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "World"}]
        }
    })
    .to_string();

    let completed = json!({
        "type": "response.completed",
        "response": { "id": "resp1" }
    })
    .to_string();

    let sse1 = format!("event: response.output_item.done\ndata: {item1}\n\n");
    let sse2 = format!("event: response.output_item.done\ndata: {item2}\n\n");
    let sse3 = format!("event: response.completed\ndata: {completed}\n\n");

    let events = collect_events(&[sse1.as_bytes(), sse2.as_bytes(), sse3.as_bytes()]).await;

    assert_eq!(events.len(), 3);

    assert_matches!(
        &events[0],
        Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
            role,
            phase: Some(MessagePhase::Commentary),
            ..
        })) if role == "assistant"
    );

    assert_matches!(
        &events[1],
        Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { role, .. }))
            if role == "assistant"
    );

    match &events[2] {
        Ok(ResponseEvent::Completed {
            response_id,
            token_usage,
        }) => {
            assert_eq!(response_id, "resp1");
            assert!(token_usage.is_none());
        }
        other => panic!("unexpected third event: {other:?}"),
    }
}

#[tokio::test]
async fn error_when_missing_completed() {
    let item1 = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Hello"}]
        }
    })
    .to_string();

    let sse1 = format!("event: response.output_item.done\ndata: {item1}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 2);

    assert_matches!(events[0], Ok(ResponseEvent::OutputItemDone(_)));

    match &events[1] {
        Err(ApiError::Stream(msg)) => {
            assert_eq!(msg, "stream closed before response.completed")
        }
        other => panic!("unexpected second event: {other:?}"),
    }
}

#[tokio::test]
async fn parses_tool_search_call_items() {
    let events = run_sse(vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "tool_search_call",
                "call_id": "search-1",
                "execution": "client",
                "arguments": {
                    "query": "calendar create",
                    "limit": 1
                }
            }
        }),
        json!({
            "type": "response.completed",
            "response": { "id": "resp1" }
        }),
    ])
    .await;

    assert_eq!(events.len(), 2);
    assert_matches!(
        &events[0],
        ResponseEvent::OutputItemDone(ResponseItem::ToolSearchCall {
            call_id,
            execution,
            arguments,
            ..
        }) if call_id.as_deref() == Some("search-1")
            && execution == "client"
            && arguments == &json!({"query": "calendar create", "limit": 1})
    );
}

#[tokio::test]
async fn parses_compaction_item_followed_by_completed() {
    let events = run_sse(vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "compaction",
                "encrypted_content": "encrypted"
            }
        }),
        json!({
            "type": "response.completed",
            "response": { "id": "resp1" }
        }),
    ])
    .await;

    assert_eq!(events.len(), 2);
    assert_matches!(
        &events[0],
        ResponseEvent::OutputItemDone(ResponseItem::Compaction {
            encrypted_content
        }) if encrypted_content == "encrypted"
    );
    assert_matches!(
        &events[1],
        ResponseEvent::Completed { response_id, .. } if response_id == "resp1"
    );
}

#[tokio::test]
async fn emits_completed_without_stream_end() {
    let completed = json!({
        "type": "response.completed",
        "response": { "id": "resp1" }
    })
    .to_string();

    let sse1 = format!("event: response.completed\ndata: {completed}\n\n");
    let stream = stream::iter(vec![Ok(Bytes::from(sse1))]).chain(stream::pending());
    let stream: ByteStream = Box::pin(stream);

    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    tokio::spawn(process_sse(stream, tx, idle_timeout(), None));

    let events = tokio::time::timeout(Duration::from_millis(1000), async {
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        events
    })
    .await
    .expect("timed out collecting events");

    assert_eq!(events.len(), 1);
    match &events[0] {
        Ok(ResponseEvent::Completed {
            response_id,
            token_usage,
        }) => {
            assert_eq!(response_id, "resp1");
            assert!(token_usage.is_none());
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn process_sse_emits_chaos_rate_limits_event_for_openai() {
    let events = run_sse_with_options(
        vec![
            json!({
                "type": "chaos.rate_limits",
                "metered_limit_name": "chaos",
                "plan_type": "pro",
                "rate_limits": {
                    "primary": {
                        "used_percent": 8.0,
                        "window_minutes": 300,
                        "reset_at": 1778843500
                    },
                    "secondary": {
                        "used_percent": 12.0,
                        "window_minutes": 10080,
                        "reset_at": 1779182173
                    }
                },
                "credits": {
                    "has_credits": false,
                    "unlimited": false,
                    "balance": null
                }
            }),
            json!({
                "type": "response.completed",
                "response": { "id": "resp1" }
            }),
        ],
        true,
    )
    .await;

    assert_eq!(events.len(), 2);
    match &events[0] {
        ResponseEvent::RateLimits(snapshot) => {
            assert_eq!(snapshot.limit_id.as_deref(), Some("chaos"));
            assert_eq!(snapshot.plan_type, Some(PlanType::Pro));
            let primary = snapshot.primary.as_ref().expect("primary");
            assert_eq!(primary.used_percent, 8.0);
            assert_eq!(primary.window_minutes, Some(300));
            let secondary = snapshot.secondary.as_ref().expect("secondary");
            assert_eq!(secondary.used_percent, 12.0);
            assert_eq!(secondary.window_minutes, Some(10080));
        }
        other => panic!("expected rate limits event, got {other:?}"),
    }
    assert_matches!(&events[1], ResponseEvent::Completed { .. });
}

#[tokio::test]
async fn process_sse_ignores_chaos_rate_limits_event_when_not_openai() {
    let events = run_sse(vec![
        json!({
            "type": "chaos.rate_limits",
            "metered_limit_name": "chaos",
            "plan_type": "pro",
            "rate_limits": {
                "primary": {
                    "used_percent": 8.0,
                    "window_minutes": 300,
                    "reset_at": 1778843500
                }
            }
        }),
        json!({
            "type": "response.completed",
            "response": { "id": "resp1" }
        }),
    ])
    .await;

    assert_eq!(events.len(), 1);
    assert_matches!(&events[0], ResponseEvent::Completed { .. });
}

#[tokio::test]
async fn error_when_error_event() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_689bcf18d7f08194bf3440ba62fe05d803fee0cdac429894","object":"response","created_at":1755041560,"status":"failed","background":false,"error":{"code":"rate_limit_exceeded","message":"Rate limit reached for gpt-5.1 in organization org-AAA on tokens per min (TPM): Limit 30000, Used 22999, Requested 12528. Please try again in 11.054s. Visit https://platform.openai.com/account/rate-limits to learn more."}, "usage":null,"user":null,"metadata":{}}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    match &events[0] {
        Err(ApiError::Retryable { message, delay }) => {
            assert_eq!(
                message,
                "Rate limit reached for gpt-5.1 in organization org-AAA on tokens per min (TPM): Limit 30000, Used 22999, Requested 12528. Please try again in 11.054s. Visit https://platform.openai.com/account/rate-limits to learn more."
            );
            assert_eq!(*delay, Some(Duration::from_secs_f64(11.054)));
        }
        other => panic!("unexpected second event: {other:?}"),
    }
}

#[tokio::test]
async fn context_window_error_is_fatal() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_5c66275b97b9baef1ed95550adb3b7ec13b17aafd1d2f11b","object":"response","created_at":1759510079,"status":"failed","background":false,"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model. Please adjust your input and try again."},"usage":null,"user":null,"metadata":{}}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    assert_matches!(events[0], Err(ApiError::ContextWindowExceeded));
}

#[tokio::test]
async fn context_window_error_with_newline_is_fatal() {
    let raw_error = r#"{"type":"response.failed","sequence_number":4,"response":{"id":"resp_fatal_newline","object":"response","created_at":1759510080,"status":"failed","background":false,"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model. Please adjust your input and try\nagain."},"usage":null,"user":null,"metadata":{}}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    assert_matches!(events[0], Err(ApiError::ContextWindowExceeded));
}

#[tokio::test]
async fn quota_exceeded_error_is_fatal() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_fatal_quota","object":"response","created_at":1759771626,"status":"failed","background":false,"error":{"code":"insufficient_quota","message":"You exceeded your current quota, please check your plan and billing details. For more information on this error, read the docs: https://platform.openai.com/docs/guides/error-codes/api-errors."},"incomplete_details":null}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    assert_matches!(events[0], Err(ApiError::QuotaExceeded));
}

#[tokio::test]
async fn invalid_prompt_without_type_is_invalid_request() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_invalid_prompt_no_type","object":"response","created_at":1759771628,"status":"failed","background":false,"error":{"code":"invalid_prompt","message":"Invalid prompt: we've limited access to this content for safety reasons."},"incomplete_details":null}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    match &events[0] {
        Err(ApiError::InvalidRequest { message }) => {
            assert_eq!(
                message,
                "Invalid prompt: we've limited access to this content for safety reasons."
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn table_driven_event_kinds() {
    struct TestCase {
        name: &'static str,
        event: serde_json::Value,
        expect_first: fn(&ResponseEvent) -> bool,
        expected_len: usize,
    }

    fn is_created(ev: &ResponseEvent) -> bool {
        matches!(ev, ResponseEvent::Created)
    }
    fn is_output(ev: &ResponseEvent) -> bool {
        matches!(ev, ResponseEvent::OutputItemDone(_))
    }
    fn is_completed(ev: &ResponseEvent) -> bool {
        matches!(ev, ResponseEvent::Completed { .. })
    }

    let completed = json!({
        "type": "response.completed",
        "response": {
            "id": "c",
            "usage": {
                "input_tokens": 0,
                "input_tokens_details": null,
                "output_tokens": 0,
                "output_tokens_details": null,
                "total_tokens": 0
            },
            "output": []
        }
    });

    let cases = vec![
        TestCase {
            name: "created",
            event: json!({"type": "response.created", "response": {}}),
            expect_first: is_created,
            expected_len: 2,
        },
        TestCase {
            name: "output_item.done",
            event: json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "hi"}
                    ]
                }
            }),
            expect_first: is_output,
            expected_len: 2,
        },
        TestCase {
            name: "unknown",
            event: json!({"type": "response.new_tool_event"}),
            expect_first: is_completed,
            expected_len: 1,
        },
    ];

    for case in cases {
        let mut evs = vec![case.event];
        evs.push(completed.clone());

        let out = run_sse(evs).await;
        assert_eq!(out.len(), case.expected_len, "case {}", case.name);
        assert!(
            (case.expect_first)(&out[0]),
            "first event mismatch in case {}",
            case.name
        );
    }
}

#[tokio::test]
async fn spawn_response_stream_emits_server_model_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        OPENAI_MODEL_HEADER,
        HeaderValue::from_static(CYBER_RESTRICTED_MODEL_FOR_TESTS),
    );
    let bytes = stream::iter(Vec::<Result<Bytes, TransportError>>::new());
    let stream_response = StreamResponse {
        status: StatusCode::OK,
        headers,
        bytes: Box::pin(bytes),
    };

    let mut stream = spawn_response_stream(stream_response, idle_timeout(), None, None, true);
    let event = stream
        .rx_event
        .recv()
        .await
        .expect("expected server model event")
        .expect("expected ok event");

    match event {
        ResponseEvent::ServerModel(model) => {
            assert_eq!(model, CYBER_RESTRICTED_MODEL_FOR_TESTS);
        }
        other => panic!("expected server model event, got {other:?}"),
    }
}

#[tokio::test]
async fn process_sse_ignores_response_model_field_in_payload() {
    let events = run_sse(vec![
        json!({
            "type": "response.created",
            "response": {
                "id": "resp-1",
                "model": CYBER_RESTRICTED_MODEL_FOR_TESTS
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp-1",
                "model": CYBER_RESTRICTED_MODEL_FOR_TESTS
            }
        }),
    ])
    .await;

    assert_eq!(events.len(), 2);
    assert_matches!(&events[0], ResponseEvent::Created);
    assert_matches!(
        &events[1],
        ResponseEvent::Completed {
            response_id,
            token_usage: None
        } if response_id == "resp-1"
    );
}

#[tokio::test]
async fn process_sse_emits_server_model_from_response_headers_payload() {
    let events = run_sse(vec![
        json!({
            "type": "response.created",
            "response": {
                "id": "resp-1",
                "headers": {
                    "OpenAI-Model": CYBER_RESTRICTED_MODEL_FOR_TESTS
                }
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp-1"
            }
        }),
    ])
    .await;

    assert_eq!(events.len(), 3);
    assert_matches!(
        &events[0],
        ResponseEvent::ServerModel(model) if model == CYBER_RESTRICTED_MODEL_FOR_TESTS
    );
    assert_matches!(&events[1], ResponseEvent::Created);
    assert_matches!(
        &events[2],
        ResponseEvent::Completed {
            response_id,
            token_usage: None
        } if response_id == "resp-1"
    );
}

#[test]
fn responses_stream_event_response_model_reads_top_level_headers() {
    let ev: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "chaos.response.metadata",
        "headers": {
            "openai-model": CYBER_RESTRICTED_MODEL_FOR_TESTS,
        }
    }))
    .expect("expected event to deserialize");

    assert_eq!(
        ev.response_model().as_deref(),
        Some(CYBER_RESTRICTED_MODEL_FOR_TESTS)
    );
}

#[test]
fn responses_stream_event_response_model_prefers_response_headers() {
    let ev: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "response.created",
        "headers": {
            "openai-model": "top-level-model"
        },
        "response": {
            "id": "resp-1",
            "headers": {
                "openai-model": CYBER_RESTRICTED_MODEL_FOR_TESTS
            }
        }
    }))
    .expect("expected event to deserialize");

    assert_eq!(
        ev.response_model().as_deref(),
        Some(CYBER_RESTRICTED_MODEL_FOR_TESTS)
    );
}

#[test]
fn test_try_parse_retry_after() {
    let err = Error {
            r#type: None,
            message: Some("Rate limit reached for gpt-5.1 in organization org- on tokens per min (TPM): Limit 1, Used 1, Requested 19304. Please try again in 28ms. Visit https://platform.openai.com/account/rate-limits to learn more.".to_string()),
            code: Some("rate_limit_exceeded".to_string()),
            plan_type: None,
            resets_at: None,
        };

    let delay = try_parse_retry_after(&err);
    assert_eq!(delay, Some(Duration::from_millis(28)));
}

#[test]
fn test_try_parse_retry_after_no_delay() {
    let err = Error {
            r#type: None,
            message: Some("Rate limit reached for gpt-5.1 in organization <ORG> on tokens per min (TPM): Limit 30000, Used 6899, Requested 24050. Please try again in 1.898s. Visit https://platform.openai.com/account/rate-limits to learn more.".to_string()),
            code: Some("rate_limit_exceeded".to_string()),
            plan_type: None,
            resets_at: None,
        };
    let delay = try_parse_retry_after(&err);
    assert_eq!(delay, Some(Duration::from_secs_f64(1.898)));
}

#[test]
fn test_try_parse_retry_after_azure() {
    let err = Error {
        r#type: None,
        message: Some("Rate limit exceeded. Try again in 35 seconds.".to_string()),
        code: Some("rate_limit_exceeded".to_string()),
        plan_type: None,
        resets_at: None,
    };
    let delay = try_parse_retry_after(&err);
    assert_eq!(delay, Some(Duration::from_secs(35)));
}

const CYBER_RESTRICTED_MODEL_FOR_TESTS: &str = "gpt-5.3-codex";
