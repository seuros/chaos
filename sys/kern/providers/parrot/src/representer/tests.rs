use super::*;
use chaos_ipc::models::FunctionCallOutputPayload;

// --- ResponsesRepresenter (real OpenAI) ---------------------------------

#[test]
fn openai_representer_remaps_system_to_developer() {
    let items = vec![ResponseItem::Message {
        id: None,
        role: "system".into(),
        content: vec![],
        end_turn: None,
        phase: None,
    }];
    let result = ResponsesRepresenter.represent(items);
    assert!(
        matches!(&result[0], ResponseItem::Message { role, .. } if role == "developer"),
        "real OpenAI must remap system → developer"
    );
}

#[test]
fn openai_representer_passes_reasoning_items_through() {
    let items = vec![ResponseItem::Reasoning {
        id: "rs_1".into(),
        summary: vec![],
        content: None,
        encrypted_content: Some("enc".into()),
    }];
    let result = ResponsesRepresenter.represent(items);
    assert_eq!(result.len(), 1, "real OpenAI keeps Reasoning items");
}

#[test]
fn openai_representer_passes_compaction_controls_through() {
    let items = vec![
        ResponseItem::CompactionTrigger {},
        ResponseItem::Compaction {
            encrypted_content: "encrypted".into(),
        },
    ];

    let result = ResponsesRepresenter.represent(items.clone());

    assert_eq!(result, items);
}

#[test]
fn openai_representer_passes_assistant_role_unchanged() {
    let items = vec![ResponseItem::Message {
        id: None,
        role: "assistant".into(),
        content: vec![],
        end_turn: None,
        phase: None,
    }];
    let result = ResponsesRepresenter.represent(items);
    assert!(matches!(&result[0], ResponseItem::Message { role, .. } if role == "assistant"));
}

// --- OpenwAInnabeRepresenter (xAI and friends) --------------------------

#[test]
fn wannabe_passes_system_role_through() {
    let items = vec![ResponseItem::Message {
        id: None,
        role: "system".into(),
        content: vec![],
        end_turn: None,
        phase: None,
    }];
    let result = OpenwAInnabeRepresenter.represent(items);
    assert!(
        matches!(&result[0], ResponseItem::Message { role, .. } if role == "system"),
        "wannabe must keep system role unchanged"
    );
}

#[test]
fn wannabe_drops_reasoning_items() {
    let items = vec![ResponseItem::Reasoning {
        id: "rs_1".into(),
        summary: vec![],
        content: None,
        encrypted_content: Some("enc".into()),
    }];
    let result = OpenwAInnabeRepresenter.represent(items);
    assert!(result.is_empty(), "wannabe must drop Reasoning items");
}

#[test]
fn wannabe_drops_compaction_controls() {
    let items = vec![
        ResponseItem::CompactionTrigger {},
        ResponseItem::Compaction {
            encrypted_content: "encrypted".into(),
        },
    ];

    let result = OpenwAInnabeRepresenter.represent(items);

    assert!(result.is_empty(), "wannabe must drop compaction controls");
}

#[test]
fn wannabe_still_converts_custom_tool_call() {
    let items = vec![ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: "call_w".into(),
        name: "apply_patch".into(),
        input: r#"{"p":"x"}"#.into(),
    }];
    let result = OpenwAInnabeRepresenter.represent(items);
    assert!(
        matches!(&result[0], ResponseItem::FunctionCall { call_id, .. } if call_id == "call_w")
    );
}

// --- Shared base behaviour ----------------------------------------------

#[test]
fn strips_tool_name_from_function_call_output() {
    let items = vec![ResponseItem::FunctionCallOutput {
        call_id: "call_1".into(),
        output: FunctionCallOutputPayload::from_text("ok".into()),
        tool_name: Some("read_file".into()),
    }];
    let result = ResponsesRepresenter.represent(items);
    match &result[0] {
        ResponseItem::FunctionCallOutput { tool_name, .. } => {
            assert!(tool_name.is_none());
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn custom_tool_call_output_becomes_function_call_output() {
    let items = vec![ResponseItem::CustomToolCallOutput {
        call_id: "call_2".into(),
        output: FunctionCallOutputPayload::from_text("patched".into()),
        tool_name: Some("apply_patch".into()),
    }];
    let result = ResponsesRepresenter.represent(items);
    match &result[0] {
        ResponseItem::FunctionCallOutput {
            call_id, tool_name, ..
        } => {
            assert_eq!(call_id, "call_2");
            assert!(tool_name.is_none());
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn custom_tool_call_becomes_function_call() {
    let items = vec![ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: "call_3".into(),
        name: "apply_patch".into(),
        input: r#"{"patch":"..."}"#.into(),
    }];
    let result = ResponsesRepresenter.represent(items);
    match &result[0] {
        ResponseItem::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        } => {
            assert_eq!(call_id, "call_3");
            assert_eq!(name, "apply_patch");
            assert_eq!(arguments, r#"{"patch":"..."}"#);
        }
        other => panic!("expected FunctionCall, got {other:?}"),
    }
}

#[test]
fn local_shell_call_becomes_function_call() {
    use chaos_ipc::models::{LocalShellAction, LocalShellExecAction, LocalShellStatus};
    let items = vec![ResponseItem::LocalShellCall {
        id: None,
        call_id: Some("sh_1".into()),
        status: LocalShellStatus::Completed,
        action: LocalShellAction::Exec(LocalShellExecAction {
            command: vec!["ls".into()],
            timeout_ms: None,
            working_directory: None,
            env: None,
            user: None,
        }),
    }];
    let result = ResponsesRepresenter.represent(items);
    match &result[0] {
        ResponseItem::FunctionCall { call_id, name, .. } => {
            assert_eq!(call_id, "sh_1");
            assert_eq!(name, "shell_command");
        }
        other => panic!("expected FunctionCall, got {other:?}"),
    }
}

// --- SessionRepresenter -------------------------------------------------

#[test]
fn session_representer_openai_remaps_system() {
    let sr = SessionRepresenter::openai();
    let items = vec![ResponseItem::Message {
        id: None,
        role: "system".into(),
        content: vec![],
        end_turn: None,
        phase: None,
    }];
    let result = sr.represent(items);
    assert!(matches!(&result[0], ResponseItem::Message { role, .. } if role == "developer"));
}

#[test]
fn session_representer_wannabe_keeps_system() {
    let sr = SessionRepresenter::wannabe();
    let items = vec![ResponseItem::Message {
        id: None,
        role: "system".into(),
        content: vec![],
        end_turn: None,
        phase: None,
    }];
    let result = sr.represent(items);
    assert!(matches!(&result[0], ResponseItem::Message { role, .. } if role == "system"));
}

#[test]
fn session_representer_is_clone() {
    let sr = SessionRepresenter::openai();
    let sr2 = sr.clone();
    // both should work identically
    let items = || {
        vec![ResponseItem::Message {
            id: None,
            role: "system".into(),
            content: vec![],
            end_turn: None,
            phase: None,
        }]
    };
    assert_eq!(sr.represent(items()).len(), sr2.represent(items()).len());
}
