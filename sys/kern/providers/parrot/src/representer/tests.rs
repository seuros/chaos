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
    std::assert_matches!(
        &result[0], ResponseItem::Message { role, .. } if role == "developer",
        "real OpenAI must remap system → developer"
    );
}

#[test]
fn provider_dialects_preserve_or_drop_foreign_reasoning_and_compaction_state() {
    let items = vec![
        ResponseItem::Reasoning {
            id: "rs_1".into(),
            summary: vec![],
            content: None,
            encrypted_content: Some("enc".into()),
        },
        ResponseItem::CompactionTrigger {},
        ResponseItem::Compaction {
            encrypted_content: "encrypted".into(),
        },
    ];

    assert_eq!(ResponsesRepresenter.represent(items.clone()), items);
    assert!(
        OpenwAInnabeRepresenter::new(SystemRole::Anywhere)
            .represent(items)
            .is_empty(),
        "compatible providers must not receive foreign encrypted state",
    );
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
    std::assert_matches!(&result[0], ResponseItem::Message { role, .. } if role == "assistant");
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
    let result = OpenwAInnabeRepresenter::new(SystemRole::Anywhere).represent(items);
    std::assert_matches!(
        &result[0], ResponseItem::Message { role, .. } if role == "system",
        "wannabe must keep system role unchanged"
    );
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
    let result = OpenwAInnabeRepresenter::new(SystemRole::Anywhere).represent(items);
    std::assert_matches!(
        &result[0], ResponseItem::FunctionCall { call_id, .. } if call_id == "call_w"
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
fn session_representer_selects_the_provider_role_policy() {
    for (representer, expected_role) in [
        (SessionRepresenter::openai(), "developer"),
        (SessionRepresenter::wannabe(), "system"),
    ] {
        assert_eq!(
            representer.represent(vec![text_message("system", "instructions")]),
            vec![text_message(expected_role, "instructions")],
        );
    }
}

// --- OpenwAInnabeRepresenter system-role switch ---------------------------

fn text_message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.into(),
        content: vec![chaos_abi::ContentItem::InputText { text: text.into() }],
        end_turn: None,
        phase: None,
    }
}

fn wannabe_request(input: Vec<ResponseItem>) -> crate::common::ResponsesApiRequest {
    crate::common::ResponsesApiRequest {
        model: "m".into(),
        instructions: "base".into(),
        input,
        tools: vec![],
        tool_choice: "auto".into(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream: true,
        include: vec![],
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        max_output_tokens: None,
    }
}

#[test]
fn leading_only_hoists_every_text_instruction_into_the_system_block() {
    let mut request = wannabe_request(vec![
        text_message("developer", "permissions"),
        text_message("system", "mode"),
        text_message("user", "hello"),
        text_message("system", "tools reminder"),
    ]);
    OpenwAInnabeRepresenter::new(SystemRole::LeadingOnly).prepare_request(&mut request);
    assert_eq!(
        request.instructions,
        "base\n\npermissions\n\nmode\n\ntools reminder"
    );
    let roles: Vec<&str> = request
        .input
        .iter()
        .map(|item| match item {
            ResponseItem::Message { role, .. } => role.as_str(),
            _ => "other",
        })
        .collect();
    assert_eq!(roles, vec!["user"]);
}

#[test]
fn leading_only_keeps_image_bearing_instruction_items_as_user() {
    let mut request = wannabe_request(vec![ResponseItem::Message {
        id: None,
        role: "system".into(),
        content: vec![chaos_abi::ContentItem::InputImage {
            image_url: "data:image/png;base64,AA==".into(),
        }],
        end_turn: None,
        phase: None,
    }]);
    OpenwAInnabeRepresenter::new(SystemRole::LeadingOnly).prepare_request(&mut request);
    assert_eq!(request.instructions, "base");
    std::assert_matches!(
        &request.input[0],
        ResponseItem::Message { role, .. } if role == "user"
    );
}

#[test]
fn anywhere_leaves_system_items_in_place() {
    let mut request = wannabe_request(vec![
        text_message("system", "mode"),
        text_message("user", "hello"),
        text_message("system", "later"),
    ]);
    OpenwAInnabeRepresenter::new(SystemRole::Anywhere).prepare_request(&mut request);
    assert_eq!(request.instructions, "base");
    assert_eq!(request.input.len(), 3);
    std::assert_matches!(
        &request.input[2],
        ResponseItem::Message { role, .. } if role == "system"
    );
}

#[test]
fn compatible_endpoint_selects_leading_only_except_xai() {
    let mut strict = wannabe_request(vec![text_message("system", "mode")]);
    SessionRepresenter::for_compatible_endpoint("http://127.0.0.1:8000/v1")
        .0
        .prepare_request(&mut strict);
    assert!(strict.input.is_empty());

    let mut xai = wannabe_request(vec![text_message("system", "mode")]);
    SessionRepresenter::for_compatible_endpoint("https://api.x.ai/v1")
        .0
        .prepare_request(&mut xai);
    assert_eq!(xai.input.len(), 1);
}
