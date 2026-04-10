use super::super::*;
use anyhow::Result;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn rollback_failed_error_does_not_affect_turn_status() {
    let event = ErrorEvent {
        message: "rollback failed".into(),
        chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
    };
    assert!(!event.affects_turn_status());
}

#[test]
fn generic_error_affects_turn_status() {
    let event = ErrorEvent {
        message: "generic".into(),
        chaos_error_info: Some(ChaosErrorInfo::Other),
    };
    assert!(event.affects_turn_status());
}

#[test]
fn user_input_serialization_omits_final_output_json_schema_when_none() -> Result<()> {
    let op = Op::UserInput {
        items: Vec::new(),
        final_output_json_schema: None,
    };

    let json_op = serde_json::to_value(op)?;
    assert_eq!(json_op, json!({ "type": "user_input", "items": [] }));

    Ok(())
}

#[test]
fn user_input_deserializes_without_final_output_json_schema_field() -> Result<()> {
    let op: Op = serde_json::from_value(json!({ "type": "user_input", "items": [] }))?;

    assert_eq!(
        op,
        Op::UserInput {
            items: Vec::new(),
            final_output_json_schema: None,
        }
    );

    Ok(())
}

#[test]
fn user_input_serialization_includes_final_output_json_schema_when_some() -> Result<()> {
    let schema = json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" }
        },
        "required": ["answer"],
        "additionalProperties": false
    });
    let op = Op::UserInput {
        items: Vec::new(),
        final_output_json_schema: Some(schema.clone()),
    };

    let json_op = serde_json::to_value(op)?;
    assert_eq!(
        json_op,
        json!({
            "type": "user_input",
            "items": [],
            "final_output_json_schema": schema,
        })
    );

    Ok(())
}

#[test]
fn user_input_text_serializes_empty_text_elements() -> Result<()> {
    let input = UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    };

    let json_input = serde_json::to_value(input)?;
    assert_eq!(
        json_input,
        json!({
            "type": "text",
            "text": "hello",
            "text_elements": [],
        })
    );

    Ok(())
}

#[test]
fn user_message_event_serializes_empty_metadata_vectors() -> Result<()> {
    let event = UserMessageEvent {
        message: "hello".to_string(),
        images: None,
        local_images: Vec::new(),
        text_elements: Vec::new(),
    };

    let json_event = serde_json::to_value(event)?;
    assert_eq!(
        json_event,
        json!({
            "message": "hello",
            "local_images": [],
            "text_elements": [],
        })
    );

    Ok(())
}

#[test]
fn turn_aborted_event_deserializes_without_turn_id() -> Result<()> {
    let event: EventMsg = serde_json::from_value(json!({
        "type": "turn_aborted",
        "reason": "interrupted",
    }))?;

    match event {
        EventMsg::TurnAborted(TurnAbortedEvent { turn_id, reason }) => {
            assert_eq!(turn_id, None);
            assert_eq!(reason, TurnAbortReason::Interrupted);
        }
        _ => panic!("expected turn_aborted event"),
    }

    Ok(())
}

#[test]
fn turn_context_item_deserializes_without_network() -> Result<()> {
    let item: TurnContextItem = serde_json::from_value(json!({
        "cwd": "/tmp",
        "approval_policy": "headless",
        "sandbox_policy": { "type": "root-access" },
        "model": "gpt-5",
        "summary": "auto",
    }))?;

    assert_eq!(item.trace_id, None);
    assert_eq!(item.network, None);
    Ok(())
}

#[test]
fn turn_context_item_serializes_network_when_present() -> Result<()> {
    let item = TurnContextItem {
        turn_id: None,
        trace_id: None,
        cwd: PathBuf::from("/tmp"),
        current_date: None,
        timezone: None,
        approval_policy: ApprovalPolicy::Headless,
        sandbox_policy: SandboxPolicy::RootAccess,
        network: Some(TurnContextNetworkItem {
            allowed_domains: vec!["api.example.com".to_string()],
            denied_domains: vec!["blocked.example.com".to_string()],
        }),
        model: "gpt-5".to_string(),
        personality: None,
        collaboration_mode: None,
        effort: None,
        summary: ReasoningSummaryConfig::Auto,
        user_instructions: None,
        minion_instructions: None,
        final_output_json_schema: None,
        truncation_policy: None,
    };

    let value = serde_json::to_value(item)?;
    assert_eq!(
        value["network"],
        json!({
            "allowed_domains": ["api.example.com"],
            "denied_domains": ["blocked.example.com"],
        })
    );
    Ok(())
}

/// Serialize Event to verify that its JSON representation has the expected
/// amount of nesting.
#[test]
fn serialize_event() -> Result<()> {
    let conversation_id = ProcessId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8")?;
    let event = Event {
        id: "1234".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: conversation_id,
            forked_from_id: None,
            process_name: None,
            model: "chaos-mini-latest".to_string(),
            model_provider_id: "openai".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: Some(ReasoningEffortConfig::default()),
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    };

    let expected = json!({
        "id": "1234",
        "msg": {
            "type": "session_configured",
            "session_id": "67e55044-10b1-426f-9247-bb680e5fe0c8",
            "model": "chaos-mini-latest",
            "model_provider_id": "openai",
            "approval_policy": "headless",
            "approvals_reviewer": "user",
            "sandbox_policy": {
                "type": "read-only"
            },
            "cwd": "/home/user/project",
            "reasoning_effort": "medium",
            "history_log_id": 0,
            "history_entry_count": 0,
        }
    });
    assert_eq!(expected, serde_json::to_value(&event)?);
    Ok(())
}

#[test]
fn vec_u8_as_base64_serialization_and_deserialization() -> Result<()> {
    let event = ExecCommandOutputDeltaEvent {
        call_id: "call21".to_string(),
        stream: ExecOutputStream::Stdout,
        chunk: vec![1, 2, 3, 4, 5],
    };
    let serialized = serde_json::to_string(&event)?;
    assert_eq!(
        r#"{"call_id":"call21","stream":"stdout","chunk":"AQIDBAU="}"#,
        serialized,
    );

    let deserialized: ExecCommandOutputDeltaEvent = serde_json::from_str(&serialized)?;
    assert_eq!(deserialized, event);
    Ok(())
}

#[test]
fn serialize_mcp_startup_update_event() -> Result<()> {
    let event = Event {
        id: "init".to_string(),
        msg: EventMsg::McpStartupUpdate(McpStartupUpdateEvent {
            server: "srv".to_string(),
            status: McpStartupStatus::Failed {
                error: "boom".to_string(),
            },
        }),
    };

    let value = serde_json::to_value(&event)?;
    assert_eq!(value["msg"]["type"], "mcp_startup_update");
    assert_eq!(value["msg"]["server"], "srv");
    assert_eq!(value["msg"]["status"]["state"], "failed");
    assert_eq!(value["msg"]["status"]["error"], "boom");
    Ok(())
}

#[test]
fn serialize_mcp_startup_complete_event() -> Result<()> {
    let event = Event {
        id: "init".to_string(),
        msg: EventMsg::McpStartupComplete(McpStartupCompleteEvent {
            ready: vec!["a".to_string()],
            failed: vec![McpStartupFailure {
                server: "b".to_string(),
                error: "bad".to_string(),
            }],
            cancelled: vec!["c".to_string()],
        }),
    };

    let value = serde_json::to_value(&event)?;
    assert_eq!(value["msg"]["type"], "mcp_startup_complete");
    assert_eq!(value["msg"]["ready"][0], "a");
    assert_eq!(value["msg"]["failed"][0]["server"], "b");
    assert_eq!(value["msg"]["failed"][0]["error"], "bad");
    assert_eq!(value["msg"]["cancelled"][0], "c");
    Ok(())
}

#[test]
fn token_usage_info_new_or_append_updates_context_window_when_provided() {
    let initial = Some(TokenUsageInfo {
        total_token_usage: TokenUsage::default(),
        last_token_usage: TokenUsage::default(),
        model_context_window: Some(258_400),
    });
    let last = Some(TokenUsage {
        input_tokens: 10,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
        total_tokens: 10,
    });

    let info = TokenUsageInfo::new_or_append(&initial, &last, Some(128_000))
        .expect("new_or_append should return info");

    assert_eq!(info.model_context_window, Some(128_000));
}

#[test]
fn token_usage_info_new_or_append_preserves_context_window_when_not_provided() {
    let initial = Some(TokenUsageInfo {
        total_token_usage: TokenUsage::default(),
        last_token_usage: TokenUsage::default(),
        model_context_window: Some(258_400),
    });
    let last = Some(TokenUsage {
        input_tokens: 10,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
        total_tokens: 10,
    });

    let info = TokenUsageInfo::new_or_append(&initial, &last, None)
        .expect("new_or_append should return info");

    assert_eq!(info.model_context_window, Some(258_400));
}
