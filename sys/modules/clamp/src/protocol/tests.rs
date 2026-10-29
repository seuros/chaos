use super::Message;
use serde_json::json;

#[test]
fn result_wire_contract_preserves_usage_and_accepts_extensions() {
    let payload = json!({
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
                "outputTokens": 19,
                "contextWindow": 200000,
                "costUSD": 0.01
            }
        }
    });
    let mut extended = payload.clone();
    extended["usage"]["cache_creation"] = json!({
        "ephemeral_5m_input_tokens": 13,
        "ephemeral_1h_input_tokens": 0
    });
    extended["usage"]["server_tool_use"] = json!({
        "web_search_requests": 1,
        "web_fetch_requests": 0
    });
    for payload in [payload, extended] {
        let message: Message = serde_json::from_value(payload).expect("result should deserialize");
        let Message::Result {
            usage, model_usage, ..
        } = message
        else {
            panic!("expected result message");
        };
        let usage = usage.expect("usage should be present");
        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.cache_creation_input_tokens, 13);
        assert_eq!(usage.cache_read_input_tokens, 17);
        assert_eq!(usage.output_tokens, 19);
        let model_usage = model_usage.expect("modelUsage should be present");
        assert_eq!(model_usage["claude-test"].context_window, Some(200_000));
        assert_eq!(
            super::observed_context_window(&model_usage, "claude-test"),
            Some(200_000)
        );
    }
}

#[test]
fn observed_context_window_prefers_requested_model_then_largest_prompt() {
    let model_usage: super::ModelUsageMap = serde_json::from_value(json!({
        "claude-haiku": {
            "inputTokens": 5,
            "cacheReadInputTokens": 2000,
            "contextWindow": 200000
        },
        "claude-opus[1m]": {
            "inputTokens": 10,
            "cacheReadInputTokens": 150000,
            "contextWindow": 1000000
        },
        "legacy": {"inputTokens": 1}
    }))
    .expect("model usage should deserialize");
    assert_eq!(
        super::observed_context_window(&model_usage, "claude-haiku"),
        Some(200_000)
    );
    assert_eq!(
        super::observed_context_window(&model_usage, "opus"),
        Some(1_000_000)
    );
    let without_window: super::ModelUsageMap =
        serde_json::from_value(json!({"legacy": {"inputTokens": 1}}))
            .expect("model usage should deserialize");
    assert_eq!(
        super::observed_context_window(&without_window, "legacy"),
        None
    );
}

#[test]
fn historical_and_error_results_without_usage_remain_unknown() {
    for (payload, expected_subtype, expected_error) in [
        (
            json!({
                "type": "result",
                "result": "ok",
                "session_id": "session-1",
                "total_cost_usd": 0.0
            }),
            None,
            false,
        ),
        (
            json!({
                "type": "result",
                "subtype": "error_during_execution",
                "is_error": true,
                "result": "execution failed",
                "session_id": "session-1"
            }),
            Some("error_during_execution"),
            true,
        ),
    ] {
        let message: Message = serde_json::from_value(payload).expect("result should deserialize");
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
        assert_eq!(subtype.as_deref(), expected_subtype);
        assert_eq!(is_error, expected_error);
    }
}
