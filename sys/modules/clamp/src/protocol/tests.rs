use super::Message;
use serde_json::json;

#[test]
fn result_preserves_usage() {
    let message: Message = serde_json::from_value(json!({
        "type": "result",
        "subtype": "success",
        "result": "ok",
        "session_id": "session-1",
        "total_cost_usd": 0.0,
        "usage": {
            "input_tokens": 11,
            "cache_creation_input_tokens": 13,
            "cache_read_input_tokens": 17,
            "output_tokens": 19,
            "service_tier": "standard"
        },
        "modelUsage": {
            "claude-test": {
                "inputTokens": 11,
                "cacheCreationInputTokens": 13,
                "cacheReadInputTokens": 17,
                "outputTokens": 19
            }
        }
    }))
    .expect("result should deserialize");

    let Message::Result { usage, .. } = message else {
        panic!("expected result message");
    };

    let usage = usage.expect("usage should be present");
    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.cache_creation_input_tokens, 13);
    assert_eq!(usage.cache_read_input_tokens, 17);
    assert_eq!(usage.output_tokens, 19);
}

#[test]
fn result_without_usage_remains_unknown() {
    let message: Message = serde_json::from_value(json!({
        "type": "result",
        "result": "ok",
        "session_id": "session-1",
        "total_cost_usd": 0.0
    }))
    .expect("historical result should deserialize");

    let Message::Result { usage, .. } = message else {
        panic!("expected result message");
    };

    assert_eq!(usage, None);
}

#[test]
fn result_usage_ignores_unknown_nested_fields() {
    let message: Message = serde_json::from_value(json!({
        "type": "result",
        "result": "ok",
        "usage": {
            "input_tokens": 11,
            "cache_creation_input_tokens": 13,
            "cache_read_input_tokens": 17,
            "output_tokens": 19,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 13,
                "ephemeral_1h_input_tokens": 0
            },
            "server_tool_use": {
                "web_search_requests": 1,
                "web_fetch_requests": 0
            },
            "service_tier": "standard"
        }
    }))
    .expect("future usage fields should not break deserialization");

    let Message::Result { usage, .. } = message else {
        panic!("expected result message");
    };

    assert!(usage.is_some());
}

#[test]
fn error_result_remains_parseable_without_usage() {
    let message: Message = serde_json::from_value(json!({
        "type": "result",
        "subtype": "error_during_execution",
        "is_error": true,
        "result": "execution failed",
        "session_id": "session-1"
    }))
    .expect("error result should deserialize");

    let Message::Result {
        usage,
        subtype,
        is_error,
        ..
    } = message
    else {
        panic!("expected result message");
    };

    assert_eq!(usage, None);
    assert_eq!(subtype.as_deref(), Some("error_during_execution"));
    assert!(is_error);
}
