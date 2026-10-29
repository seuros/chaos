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
            super::observed_context_window(&model_usage, None, "claude-test"),
            Some(200_000)
        );
    }
}

#[test]
fn observed_context_window_follows_the_served_model_then_the_requested_slug() {
    let model_usage: super::ModelUsageMap = serde_json::from_value(json!({
        "claude-haiku-4-5-20251001": {
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
    // The served model is Claude Code's own name for the main-thread model,
    // which can differ from the requested slug.
    assert_eq!(
        super::observed_context_window(&model_usage, Some("claude-haiku-4-5-20251001"), "haiku"),
        Some(200_000)
    );
    // Without a served model only an exact match on the requested slug counts;
    // the helper that saw the most tokens is never used.
    assert_eq!(
        super::observed_context_window(&model_usage, None, "claude-opus[1m]"),
        Some(1_000_000)
    );
    assert_eq!(
        super::observed_context_window(&model_usage, None, "opus"),
        None
    );
    // A served model without a reported window does not inherit a helper's.
    assert_eq!(
        super::observed_context_window(&model_usage, Some("legacy"), "legacy"),
        None
    );
    let without_window: super::ModelUsageMap =
        serde_json::from_value(json!({"legacy": {"inputTokens": 1}}))
            .expect("model usage should deserialize");
    assert_eq!(
        super::observed_context_window(&without_window, Some("legacy"), "legacy"),
        None
    );
}

#[test]
fn assistant_messages_carry_the_sidechain_marker() {
    let main: Message = serde_json::from_value(json!({
        "type": "assistant",
        "message": {"model": "claude-haiku-4-5-20251001", "content": []},
        "parent_tool_use_id": null,
        "session_id": "session-1"
    }))
    .expect("assistant should deserialize");
    let Message::Assistant {
        message,
        parent_tool_use_id,
    } = main
    else {
        panic!("expected assistant message");
    };
    assert_eq!(message["model"], "claude-haiku-4-5-20251001");
    assert_eq!(parent_tool_use_id, None);

    let sidechain: Message = serde_json::from_value(json!({
        "type": "assistant",
        "message": {"model": "claude-haiku-4-5-20251001", "content": []},
        "parent_tool_use_id": "toolu_01"
    }))
    .expect("assistant should deserialize");
    let Message::Assistant {
        parent_tool_use_id, ..
    } = sidechain
    else {
        panic!("expected assistant message");
    };
    assert_eq!(parent_tool_use_id.as_deref(), Some("toolu_01"));
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
