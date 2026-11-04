use super::super::*;
use chaos_abi::AbiError;
use chaos_abi::TurnEvent;
use chaos_test_fixtures::TEST_MODEL;
use rama::bytes::Bytes;
use rama::futures;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::fixtures::completion_fixture;
#[tokio::test]
async fn json_body_errors_do_not_emit_success() {
    let (tx, mut rx) = mpsc::channel(16);
    let result = process_json_response(
        rama::http::Body::from("not json"),
        Duration::from_secs(1),
        tx,
    )
    .await;
    std::assert_matches!(result, Err(AbiError::Stream(_)));
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
                    "data: {\"id\":\"chatcmpl-test\",",
                    "\"choices\":[{\"delta\":{\"content\":\"hello\"},",
                    "\"finish_reason\":\"stop\"}],\"usage\":null}\n\n",
                    "data: {\"choices\":[],",
                    "\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,",
                    "\"total_tokens\":17}}\n\n",
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
        std::assert_matches!(events.first(), Some(TurnEvent::Created));
        std::assert_matches!(
            events.last(),
            Some(TurnEvent::Completed { token_usage: Some(usage), .. })
                if usage.total_tokens == 17
        );
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
