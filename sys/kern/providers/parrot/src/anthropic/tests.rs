use super::*;
use futures::stream;
use rama::http::header::AUTHORIZATION;
use serde_json::Map;
use serde_json::json;
use std::time::Duration;

fn test_request() -> TurnRequest {
    TurnRequest {
        model: "claude-test".to_string(),
        instructions: "Be helpful.".to_string(),
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Hello".to_string(),
            }],
            phase: None,
            end_turn: None,
        }],
        tools: Vec::new(),
        parallel_tool_calls: true,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: Map::new(),
    }
}

#[test]
fn output_cap_overrides_legacy_limit_without_changing_ordinary_anthropic_payload() {
    let mut request = test_request();
    request
        .extensions
        .insert(CACHE_TTL_EXTENSION.into(), json!("off"));
    let baseline = build_request_body(&request, "claude-test").unwrap();
    assert_eq!(
        baseline,
        json!({
            "model": "claude-test",
            "max_tokens": DEFAULT_MAX_TOKENS,
            "stream": true,
            "system": "Be helpful.",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "Hello"}]}]
        })
    );
    request.extensions.insert("max_tokens".into(), json!(1234));
    assert_eq!(
        build_request_body(&request, "claude-test").unwrap()["max_tokens"],
        1234
    );
    for cap in [1, 37, u32::MAX] {
        request
            .extensions
            .insert("max_output_tokens".into(), json!(cap));
        let mut body = build_request_body(&request, "claude-test").unwrap();
        assert_eq!(body["max_tokens"], cap);
        assert!(body.get("max_output_tokens").is_none());
        body["max_tokens"] = json!(DEFAULT_MAX_TOKENS);
        assert_eq!(
            serde_json::to_vec(&body).unwrap(),
            serde_json::to_vec(&baseline).unwrap()
        );
    }
}

fn test_provider() -> Provider {
    use crate::provider::RetryConfig;

    Provider {
        egress: None,
        name: "Anthropic".to_string(),
        base_url: chaos_services::anthropic::API_BASE.to_string(),
        query_params: None,
        headers: HeaderMap::new(),
        retry: RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: true,
            retry_5xx: true,
            retry_transport: true,
        },
        stream_idle_timeout: Duration::from_millis(10),
    }
}

#[test]
fn build_headers_selects_one_authentication_scheme() {
    for (auth, api_key, bearer) in [
        (
            AnthropicAuth::ApiKey("sk-ant".to_string()),
            Some("sk-ant"),
            None,
        ),
        (
            AnthropicAuth::BearerToken("tok-ant".to_string()),
            None,
            Some("Bearer tok-ant"),
        ),
    ] {
        let adapter = AnthropicAdapter::new(test_provider(), auth, None);
        let headers = adapter.build_headers().expect("headers should build");
        assert_eq!(
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            api_key,
        );
        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            bearer,
        );
    }
}

#[test]
fn build_request_body_applies_prompt_cache_policy() {
    for (ttl, expected) in [
        (None, Some(json!({"type": "ephemeral"}))),
        (Some("1h"), Some(json!({"type": "ephemeral", "ttl": "1h"}))),
        (Some("off"), None),
    ] {
        let mut request = test_request();
        if let Some(ttl) = ttl {
            request
                .extensions
                .insert(CACHE_TTL_EXTENSION.to_string(), json!(ttl));
        }
        let body = build_request_body(&request, "claude-test").expect("request should build");
        assert_eq!(body.get("cache_control"), expected.as_ref(), "{ttl:?}");
    }
}

#[tokio::test]
async fn model_discovery_maps_anthropics_native_metadata() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let gateway = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/egress/chaos/v1/models"))
        .and(header("x-lsd-upstream", "https://api.anthropic.com"))
        .and(header("x-api-key", "test-key"))
        .and(header(ANTHROPIC_VERSION_HEADER, ANTHROPIC_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "type": "model",
                "id": "claude-catalog-test",
                "display_name": "Claude Catalog Test",
                "created_at": "2026-01-01T00:00:00Z",
                "max_input_tokens": 200000,
                "max_tokens": 8192,
                "capabilities": {
                    "thinking": {"supported": true, "types": {"adaptive": true}},
                    "image_input": {"supported": true},
                    "structured_outputs": {"supported": true},
                    "effort": {"supported": false}
                }
            }],
            "has_more": false,
            "first_id": "claude-catalog-test",
            "last_id": "claude-catalog-test"
        })))
        .mount(&gateway)
        .await;
    let mut provider = test_provider();
    provider.egress = Some(
        chaos_client::Egress::parse(&format!("{}/egress/chaos", gateway.uri()))
            .expect("test gateway"),
    );
    let adapter = AnthropicAdapter::new(provider, AnthropicAuth::ApiKey("test-key".into()), None);

    let models = adapter.list_models().await.expect("native discovery");
    assert_eq!(models.len(), 1);
    let model = &models[0];
    assert_eq!(model.id, "claude-catalog-test");
    assert_eq!(model.display_name, "Claude Catalog Test");
    assert_eq!(model.max_input_tokens, Some(200000));
    assert_eq!(model.max_output_tokens, Some(8192));
    assert!(model.supports_thinking);
    assert!(model.supports_images);
    assert!(model.supports_structured_output);
    assert!(!model.supports_reasoning_effort);
}

#[test]
fn streamed_usage_combines_message_start_cache_usage_and_message_delta_output() {
    let mut tool_acc = None;
    let mut text_acc = None;
    let mut usage_acc = UsageAccumulator::default();

    let start_events = parse_sse_event(
        "message_start",
        &json!({
            "message": {
                "id": "msg-usage-receipt",
                "model": "claude-test",
                "usage": {
                    "input_tokens": 11,
                    "cache_creation_input_tokens": 13,
                    "cache_read_input_tokens": 17,
                    "output_tokens": 1
                }
            }
        }),
        &mut tool_acc,
        &mut text_acc,
        &mut usage_acc,
    )
    .expect("message_start should parse");
    assert_eq!(start_events.len(), 2);
    std::assert_matches!(start_events[0], TurnEvent::Created);
    std::assert_matches!(
        &start_events[1],
        TurnEvent::ServerModel(model) if model == "claude-test"
    );

    let completed = parse_sse_event(
        "message_delta",
        &json!({
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 19}
        }),
        &mut tool_acc,
        &mut text_acc,
        &mut usage_acc,
    )
    .expect("message_delta should parse");

    assert_eq!(completed.len(), 1);
    let TurnEvent::Completed {
        response_id,
        token_usage: Some(usage),
    } = &completed[0]
    else {
        panic!("expected one completed event with token usage");
    };
    assert_eq!(response_id, "msg-usage-receipt");
    assert_eq!(usage.input_tokens, 41);
    assert_eq!(usage.cache_creation_input_tokens, 13);
    assert_eq!(usage.cached_input_tokens, 17);
    assert_eq!(usage.output_tokens, 19);
    assert_eq!(usage.total_tokens, 60);
}

#[tokio::test]
async fn process_sse_data_stream_times_out_when_idle() {
    let (tx, _rx) = mpsc::channel(4);
    let stream = stream::pending::<Result<Bytes, std::io::Error>>();

    let err = process_sse_data_stream(stream, Duration::from_millis(5), tx)
        .await
        .expect_err("idle stream should time out");

    std::assert_matches!(err, AbiError::Stream(message) if message == "idle timeout waiting for SSE");
}

#[test]
fn missing_or_partial_streamed_usage_is_not_a_fabricated_zero_receipt() {
    for start_usage in [json!(null), json!({"input_tokens": 12})] {
        let mut tools = None;
        let mut text = None;
        let mut usage = UsageAccumulator::default();
        parse_sse_event(
            "message_start",
            &json!({"message": {"usage": start_usage}}),
            &mut tools,
            &mut text,
            &mut usage,
        )
        .unwrap();
        let events = parse_sse_event(
            "message_delta",
            &json!({"delta": {"stop_reason": "end_turn"}}),
            &mut tools,
            &mut text,
            &mut usage,
        )
        .unwrap();
        assert!(matches!(
            &events[0],
            TurnEvent::Completed {
                token_usage: None,
                ..
            }
        ));
    }
}

#[test]
fn document_content_preserves_source_and_optional_title() {
    for name in [None, Some("notes.txt")] {
        let content = ContentItem::Document {
            name: name.map(str::to_string),
            mime_type: "text/plain".to_string(),
            text: "document body".to_string(),
        };
        let value = convert_content_item(&content).expect("document should convert");
        assert_eq!(value["type"], "document");
        assert_eq!(value.get("title"), name.map(|name| json!(name)).as_ref());
        assert_eq!(value["source"]["type"], "text");
        assert_eq!(value["source"]["media_type"], "text/plain");
        assert_eq!(value["source"]["data"], "document body");
    }
}
