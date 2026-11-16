use chaos_abi::{AbiError, ContinuationSlot, ModelAdapter, TurnEvent, TurnRequest};
use chaos_ipc::models::{
    ContentItem, FunctionCallOutputBody, FunctionCallOutputPayload, ResponseItem,
};
use lsd_protocol::fixtures::{CHUNK_STREAMS, chunk_lines};
use rama::futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TRIP: &str = "01938d2a-7c4e-7a10-9f5e-1b2c3d4e5f60";

fn sse_reply() -> ResponseTemplate {
    let (_, stream) = CHUNK_STREAMS[0];
    let mut body: String = chunk_lines(stream)
        .map(|line| format!("data: {line}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
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

fn turn(input: Vec<ResponseItem>, slot: &ContinuationSlot) -> TurnRequest {
    TurnRequest {
        ephemeral: Vec::new(),
        continuation: Some(slot.clone()),
        model: "coding_large".into(),
        instructions: "Be terse.".into(),
        input,
        tools: vec![],
        parallel_tool_calls: true,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: Default::default(),
    }
}

/// The history the kernel holds after the golden reply, plus the tool result.
fn after_reply(reply: Vec<ResponseItem>) -> Vec<ResponseItem> {
    let mut input = vec![user("where am I?")];
    input.extend(reply);
    input.push(ResponseItem::FunctionCallOutput {
        call_id: "call_3".into(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("/home/chaos".into()),
            success: Some(true),
        },
        tool_name: Some("shell".into()),
    });
    input
}

async fn run(
    adapter: &dyn ModelAdapter,
    request: TurnRequest,
) -> Result<Vec<ResponseItem>, AbiError> {
    let mut stream = adapter.stream(request).await?;
    let mut done = Vec::new();
    while let Some(event) = stream.next().await {
        match event? {
            TurnEvent::OutputItemDone(item) => done.push(item),
            TurnEvent::Completed { .. } => break,
            _ => {}
        }
    }
    Ok(done)
}

async fn bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap_or(Value::Null))
        .collect()
}

#[tokio::test]
async fn the_second_request_of_a_turn_continues_the_trip_with_only_the_tool_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/inference"))
        .respond_with(sse_reply())
        .mount(&server)
        .await;
    let adapter = chaos_parrot::adapter_for_wire("lsd", server.uri(), String::new(), None)
        .unwrap_or_else(|| panic!("LSD wire adapter"));
    let slot = ContinuationSlot::default();

    let reply = run(adapter.as_ref(), turn(vec![user("where am I?")], &slot))
        .await
        .unwrap_or_else(|e| panic!("first request: {e}"));
    run(adapter.as_ref(), turn(after_reply(reply), &slot))
        .await
        .unwrap_or_else(|e| panic!("second request: {e}"));

    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2);
    assert!(sent[0].get("continue_trip").is_none());
    assert_eq!(sent[1]["continue_trip"], true);
    assert_eq!(sent[1]["trip_id"], TRIP);
    assert_eq!(
        sent[1]["input"]["messages"],
        json!([{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "id": "call_3",
                "name": "shell",
                "result": "/home/chaos"
            }]
        }])
    );
    assert_eq!(sent[1]["input"]["system"], "Be terse.");
}

#[tokio::test]
async fn a_lost_trip_is_replayed_in_full_under_the_same_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/inference"))
        .and(body_partial_json(json!({"continue_trip": true})))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": "trip cannot be continued",
            "error_code": "trip_not_found"
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/inference"))
        .respond_with(sse_reply())
        .with_priority(2)
        .mount(&server)
        .await;
    let adapter = chaos_parrot::adapter_for_wire("lsd", server.uri(), String::new(), None)
        .unwrap_or_else(|| panic!("LSD wire adapter"));
    let slot = ContinuationSlot::default();

    let reply = run(adapter.as_ref(), turn(vec![user("where am I?")], &slot))
        .await
        .unwrap_or_else(|e| panic!("first request: {e}"));
    let history = after_reply(reply);
    run(adapter.as_ref(), turn(history, &slot))
        .await
        .unwrap_or_else(|e| panic!("replayed request: {e}"));

    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[1]["continue_trip"], true);
    assert!(sent[2].get("continue_trip").is_none());
    assert_eq!(sent[2]["trip_id"], TRIP);
    let replayed = sent[2]["input"]["messages"].as_array().map_or(0, Vec::len);
    assert_eq!(replayed, 3, "user, the stored reply, and the tool result");
}
