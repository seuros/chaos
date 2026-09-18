use chaos_abi::{
    AbiError, ContentItem, FunctionToolDef, ModelAdapter, ReasoningConfig, ReasoningEffort,
    ReasoningSummary, ResponseItem, ToolDef, TurnEvent, TurnRequest,
};
use chaos_client::{Egress, RamaTransport};
use chaos_ipc::models::FunctionCallOutputPayload;
use chaos_parrot::openai::{OpenAiAdapter, StaticAuthProvider};
use chaos_parrot::{Provider, SessionRepresenter};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn turn(model: &str, input: Vec<ResponseItem>) -> TurnRequest {
    TurnRequest {
        model: model.into(),
        instructions: "Inspect the project.".into(),
        input,
        tools: vec![ToolDef::Function(FunctionToolDef {
            name: "read_file".into(),
            description: "Read a project file.".into(),
            parameters: json!({"type": "object", "properties": {}}),
            strict: false,
        })],
        parallel_tool_calls: true,
        reasoning: Some(ReasoningConfig {
            effort: Some(ReasoningEffort::Medium),
            summary: Some(ReasoningSummary::Auto),
        }),
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: Default::default(),
    }
}

fn sse(items: &[Value]) -> ResponseTemplate {
    let mut events = vec![json!({
        "type": "response.created",
        "response": {"id": "resp_kimi"}
    })];
    events.push(json!({
        "type": "response.reasoning_summary_text.delta",
        "item_id": "rs_kimi",
        "summary_index": 0,
        "delta": "Inspect the files."
    }));
    events.extend(items.iter().map(|item| {
        json!({
            "type": "response.output_item.done",
            "item": item
        })
    }));
    events.push(json!({
        "type": "response.completed",
        "response": {
            "id": "resp_kimi",
            "status": "completed",
            "usage": {
                "input_tokens": 25,
                "input_tokens_details": {"cached_tokens": 10},
                "output_tokens": 12,
                "output_tokens_details": {"reasoning_tokens": 8},
                "total_tokens": 37
            }
        }
    }));
    let body: String = events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect();
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

async fn collect(
    adapter: &OpenAiAdapter<StaticAuthProvider>,
    request: TurnRequest,
) -> Vec<TurnEvent> {
    let mut stream = adapter.stream(request).await.unwrap();
    let mut events = Vec::new();
    while let Some(event) = timeout(Duration::from_secs(5), stream.rx_event.recv())
        .await
        .unwrap()
    {
        events.push(event.unwrap());
    }
    events
}

#[tokio::test]
async fn kimi_discovery_and_reasoning_tool_round_trip_use_the_selected_endpoint() {
    for (base_url, model, upstream, api_path) in [
        (
            "https://api.moonshot.ai/v1",
            "kimi-k3",
            "https://api.moonshot.ai",
            "/v1",
        ),
        (
            "https://api.kimi.ai/coding/v1",
            "kimi-for-coding",
            "https://api.kimi.ai",
            "/coding/v1",
        ),
    ] {
        // Keep the real endpoint identity while routing every byte to a local
        // mock. No vendor access or credentials are needed.
        let gateway = MockServer::start().await;
        let mut provider = Provider::from_base_url_with_default_streaming_config(
            "Moonshot AI",
            base_url.into(),
            false,
        );
        provider.egress = Some(Egress::parse(&format!("{}/egress/chaos", gateway.uri())).unwrap());
        provider.retry.max_attempts = 1;
        let adapter = OpenAiAdapter::new(
            RamaTransport::default_client_with_egress(provider.egress.clone()),
            provider,
            StaticAuthProvider::new(Some("kimi-test-key".into()), None),
            None,
            SessionRepresenter::for_compatible_endpoint(base_url),
        );

        Mock::given(method("GET"))
            .and(path(format!("/egress/chaos{api_path}/models")))
            .and(header("authorization", "Bearer kimi-test-key"))
            .and(header("x-lsd-upstream", upstream))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [{
                    "id": model,
                    "owned_by": "moonshot",
                    "context_length": 1_048_576,
                    "supports_image_in": true,
                    "supports_reasoning": true
                }]
            })))
            .expect(1)
            .mount(&gateway)
            .await;
        let models = adapter.list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, model);
        assert_eq!(models[0].max_input_tokens, Some(1_048_576));
        assert!(models[0].supports_images);
        assert!(models[0].supports_thinking);
        gateway.verify().await;
        gateway.reset().await;

        let reasoning = json!({
            "id": "rs_kimi",
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "Inspect the files."}],
            "encrypted_content": null
        });
        let call = json!({
            "id": "fc_kimi",
            "type": "function_call",
            "call_id": "call_kimi",
            "name": "read_file",
            "arguments": "{}"
        });
        let responses_path = format!("/egress/chaos{api_path}/responses");
        Mock::given(method("POST"))
            .and(path(&responses_path))
            .and(header("authorization", "Bearer kimi-test-key"))
            .and(header("x-lsd-upstream", upstream))
            .respond_with(sse(&[reasoning.clone(), call]))
            .expect(1)
            .mount(&gateway)
            .await;

        let mut request = turn(model, vec![]);
        request.reasoning.as_mut().unwrap().effort = Some(ReasoningEffort::None);
        let events = collect(&adapter, request).await;
        let requests = gateway.received_requests().await.unwrap();
        let body: Value = requests[0].body_json().unwrap();
        assert_eq!(body["reasoning"], json!({"effort": "low"}));
        assert!(events.iter().any(|event| matches!(
            event,
            TurnEvent::ReasoningSummaryDelta { delta, .. } if delta == "Inspect the files."
        )));
        assert!(matches!(events.last(),
            Some(TurnEvent::Completed { token_usage: Some(usage), .. })
                if usage.total_tokens == 37 && usage.reasoning_output_tokens == 8
                    && usage.cached_input_tokens == 10
        ));
        let mut history: Vec<ResponseItem> = events
            .into_iter()
            .filter_map(|event| match event {
                TurnEvent::OutputItemDone(item) => Some(item),
                _ => None,
            })
            .collect();
        assert_eq!(history.len(), 2);
        history.push(ResponseItem::FunctionCallOutput {
            call_id: "call_kimi".into(),
            output: FunctionCallOutputPayload::from_text("project files".into()),
            tool_name: Some("read_file".into()),
        });
        gateway.verify().await;
        gateway.reset().await;

        Mock::given(method("POST"))
            .and(path(&responses_path))
            .and(header("authorization", "Bearer kimi-test-key"))
            .and(header("x-lsd-upstream", upstream))
            .respond_with(sse(&[json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Done."}]
            })]))
            .expect(1)
            .mount(&gateway)
            .await;
        let events = collect(&adapter, turn(model, history)).await;
        assert!(events.iter().any(|event| matches!(
            event,
            TurnEvent::OutputItemDone(ResponseItem::Message { content, .. })
                if matches!(content.as_slice(), [ContentItem::OutputText { text }] if text == "Done.")
        )));
        let requests = gateway.received_requests().await.unwrap();
        let body: Value = requests[0].body_json().unwrap();
        assert_eq!(body["model"], model);
        assert_eq!(body["reasoning"], json!({"effort": "high"}));
        assert_eq!(body["include"], json!([]));
        assert_eq!(body["input"][0]["summary"], reasoning["summary"]);
        assert_eq!(body["input"][1]["call_id"], "call_kimi");
        assert_eq!(body["input"][2]["call_id"], "call_kimi");
        assert_eq!(body["input"][2]["output"], "project files");
        assert!(body["input"][2].get("tool_name").is_none());
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert!(requests[0].headers.get("chatgpt-account-id").is_none());
        gateway.verify().await;
        gateway.reset().await;

        Mock::given(method("POST"))
            .and(path(&responses_path))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "invalid Kimi key"}
            })))
            .expect(1)
            .mount(&gateway)
            .await;
        assert!(matches!(
            adapter.stream(turn(model, vec![])).await,
            Err(AbiError::Transport { status: 401, .. })
        ));
        gateway.verify().await;
    }
}
