use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_kern::ResponseEvent;
use lsd_protocol::ContentBlockChunk;
use lsd_protocol::ErrorBody;
use lsd_protocol::ErrorCode;
use lsd_protocol::FinishReason;
use lsd_protocol::InferenceRequest;
use lsd_protocol::OutputContent;
use lsd_protocol::fixtures::REQUESTS;
use rama::Service;
use rama::http::Body;
use rama::http::Request;
use rama::http::Response;
use rama::http::StatusCode;
use rama::http::body::util::BodyExt;
use serde_json::json;

use super::convert::Projection;
use super::convert::Step;
use super::convert::turn;
use crate::test_support::server_state;

fn post(body: serde_json::Value, token: Option<&str>) -> Request {
    let mut builder = Request::builder()
        .method("POST")
        .uri(lsd_protocol::INFERENCE_PATH)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn send(request: Request) -> (StatusCode, ErrorBody) {
    let (_home, state) = server_state(64 * 1024, false).await;
    let response: Response = crate::api::http_service(state)
        .serve(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: ErrorBody = serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("protocol error body expected, got {bytes:?}: {e}"));
    (status, body)
}

fn minimal(extra: serde_json::Value) -> serde_json::Value {
    let mut body = json!({
        "model_name": "openai::gpt-5",
        "input": {"messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]}
    });
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    body
}

#[tokio::test]
async fn refusals_speak_the_protocol_error_shape() {
    let (status, body) = send(post(minimal(json!({})), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body.error_code, ErrorCode::Unauthorized);

    let (status, body) = send(post(
        json!({"input": {}, "surprise": 1}),
        Some("test-token"),
    ))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body.error_code, ErrorCode::InvalidRequest);

    let (status, body) = send(post(
        minimal(json!({"variant_name": "v1"})),
        Some("test-token"),
    ))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.error.contains("variants"));
}

#[tokio::test]
async fn chaos_keeps_no_trips_so_a_continuation_must_be_replayed() {
    let (status, body) = send(post(
        minimal(json!({"continue_trip": true, "trip_id": "01938d2a-7c4e-7a10-9f5e-1b2c3d4e5f60"})),
        Some("test-token"),
    ))
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_trip_not_found());
}

#[tokio::test]
async fn an_unconfigured_provider_is_an_unknown_target() {
    let (status, body) = send(post(
        minimal(json!({"model_name": "nowhere::model"})),
        Some("test-token"),
    ))
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body.error_code, ErrorCode::UnknownTarget);
}

#[tokio::test]
async fn only_post_is_served() {
    let (_home, state) = server_state(1024, false).await;
    let request = Request::builder()
        .method("GET")
        .uri(lsd_protocol::INFERENCE_PATH)
        .body(Body::empty())
        .unwrap();
    let response: Response = crate::api::http_service(state)
        .serve(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers().get("allow").unwrap(), "POST");
}

#[test]
fn the_golden_agent_turn_becomes_kernel_history() {
    let (_, json) = REQUESTS[0];
    let request: InferenceRequest = serde_json::from_str(json).unwrap();
    let turn = turn(&request).unwrap();
    assert_eq!(turn.instructions, "You are a careful coding agent.");
    assert_eq!(turn.tools.len(), 1);
    assert!(turn.parallel_tool_calls);
    std::assert_matches!(
        &turn.input[1],
        ResponseItem::Reasoning { encrypted_content: Some(signature), .. } if signature == "sig-abc"
    );
    std::assert_matches!(
        &turn.input[3],
        ResponseItem::FunctionCall { call_id, .. } if call_id == "call_1"
    );
    std::assert_matches!(
        &turn.input[4],
        ResponseItem::FunctionCallOutput { output, .. } if output.success == Some(true)
    );
    assert!(
        matches!(turn.input.last(), Some(ResponseItem::Message { role, .. }) if role == "developer")
    );
}

#[test]
fn forced_tool_choice_is_refused() {
    let mut request: InferenceRequest = serde_json::from_value(minimal(json!({}))).unwrap();
    request.tool_choice = Some(lsd_protocol::ToolChoice::Required);
    assert!(turn(&request).is_err());
}

#[test]
fn kernel_events_project_to_chunks_and_a_tool_call_finish() {
    let mut projection = Projection::default();
    let Step::Content(text) = projection.step(ResponseEvent::OutputTextDelta("Hi".into())) else {
        panic!("a text delta streams");
    };
    assert!(matches!(&text[0], ContentBlockChunk::Text { text, .. } if text == "Hi"));
    assert!(matches!(
        projection.step(ResponseEvent::OutputItemDone(ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![ContentItem::OutputText { text: "Hi".into() }],
            end_turn: None,
            phase: None,
        })),
        Step::Nothing
    ));
    let Step::Content(call) =
        projection.step(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
            id: None,
            name: "shell".into(),
            namespace: None,
            arguments: r#"{"command":"ls"}"#.into(),
            call_id: "c1".into(),
            provider_metadata: None,
        }))
    else {
        panic!("a finished tool call streams");
    };
    assert!(matches!(&call[0], ContentBlockChunk::ToolCall { id, .. } if id == "c1"));
    let Step::Finished { finish_reason, .. } = projection.step(ResponseEvent::Completed {
        response_id: "r".into(),
        token_usage: None,
    }) else {
        panic!("completion finishes the reply");
    };
    assert_eq!(finish_reason, FinishReason::ToolCall);
    assert!(
        matches!(&projection.output[1], OutputContent::ToolCall(call) if call.arguments.is_some())
    );
}
