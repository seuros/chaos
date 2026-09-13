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
fn build_headers_uses_x_api_key_for_api_key_auth() {
    let adapter = AnthropicAdapter::new(
        test_provider(),
        AnthropicAuth::ApiKey("sk-ant".to_string()),
        None,
    );

    let headers = adapter.build_headers().expect("headers should build");

    assert_eq!(
        headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("sk-ant")
    );
    assert!(headers.get(AUTHORIZATION).is_none());
}

#[test]
fn build_headers_uses_bearer_for_bearer_auth() {
    let adapter = AnthropicAdapter::new(
        test_provider(),
        AnthropicAuth::BearerToken("tok-ant".to_string()),
        None,
    );

    let headers = adapter.build_headers().expect("headers should build");

    assert_eq!(
        headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer tok-ant")
    );
    assert!(headers.get("x-api-key").is_none());
}

#[test]
fn build_request_body_enables_automatic_prompt_caching() {
    let request = test_request();

    let body = build_request_body(&request, "claude-test", "5m").expect("request should build");

    assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
}

#[test]
fn build_request_body_supports_one_hour_cache_ttl() {
    let mut request = test_request();
    request
        .extensions
        .insert(CACHE_TTL_EXTENSION.to_string(), json!("1h"));

    let body = build_request_body(&request, "claude-test", "off").expect("request should build");

    assert_eq!(
        body["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
}

#[test]
fn build_request_body_can_disable_prompt_caching() {
    let mut request = test_request();
    request
        .extensions
        .insert(CACHE_TTL_EXTENSION.to_string(), json!("off"));

    let body = build_request_body(&request, "claude-test", "5m").expect("request should build");

    assert!(body.get("cache_control").is_none());
}

#[test]
fn prompt_caching_defaults_on_only_for_official_anthropic_endpoint() {
    assert_eq!(
        default_cache_ttl_for_base_url("https://api.anthropic.com/v1"),
        "5m"
    );
    assert_eq!(
        default_cache_ttl_for_base_url("https://api.minimax.io/anthropic"),
        "off"
    );
    assert_eq!(
        default_cache_ttl_for_base_url("https://api.moonshot.ai/anthropic"),
        "off"
    );
    assert_eq!(
        default_cache_ttl_for_base_url("https://api.z.ai/api/anthropic"),
        "off"
    );
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
    assert!(matches!(start_events[0], TurnEvent::Created));
    assert!(matches!(
        &start_events[1],
        TurnEvent::ServerModel(model) if model == "claude-test"
    ));

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
    assert!(response_id.is_empty());
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

    assert!(matches!(err, AbiError::Stream(message) if message == "idle timeout waiting for SSE"));
}

#[test]
fn document_content_without_name_omits_title() {
    let content = ContentItem::Document {
        name: None,
        mime_type: "text/plain".to_string(),
        text: "document body".to_string(),
    };

    let value = convert_content_item(&content).expect("document should convert");

    assert_eq!(value["type"], "document");
    assert!(value.get("title").is_none());
    assert_eq!(value["source"]["type"], "text");
    assert_eq!(value["source"]["media_type"], "text/plain");
    assert_eq!(value["source"]["data"], "document body");
}

#[test]
fn document_content_with_name_sets_title() {
    let content = ContentItem::Document {
        name: Some("notes.txt".to_string()),
        mime_type: "text/plain".to_string(),
        text: "document body".to_string(),
    };

    let value = convert_content_item(&content).expect("document should convert");

    assert_eq!(value["title"], "notes.txt");
}
