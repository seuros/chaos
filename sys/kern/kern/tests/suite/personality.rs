use chaos_ipc::openai_models::ConfigShellToolType;
use chaos_ipc::openai_models::ModelInstructionsVariables;
use chaos_ipc::openai_models::ModelMessages;
use chaos_ipc::openai_models::ModelVisibility;
use chaos_ipc::openai_models::ModelsResponse;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::user_input::UserInput;
use chaos_kern::config::types::Personality;
use chaos_kern::test_support::test_remote_model;
use chaos_kern::test_support::wait_for_model_available;
use core_test_support::load_default_config_for_test;
use core_test_support::responses::mount_models_once;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse_completed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use wiremock::BodyPrintLimit;
use wiremock::MockServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn personality_does_not_mutate_base_instructions_without_template() {
    let chaos_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&chaos_home).await;
    config.personality = Some(Personality::Friendly);

    let model_info = chaos_kern::test_support::construct_model_info_offline("gpt-5.1", &config);
    assert_eq!(
        model_info.get_model_instructions(config.personality),
        model_info.base_instructions
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn base_instructions_override_disables_personality_template() {
    let chaos_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&chaos_home).await;
    config.personality = Some(Personality::Friendly);
    config.base_instructions = Some("override instructions".to_string());

    let model_info =
        chaos_kern::test_support::construct_model_info_offline("gpt-5.2-codex", &config);

    assert_eq!(model_info.base_instructions, "override instructions");
    assert_eq!(
        model_info.get_model_instructions(config.personality),
        "override instructions"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_turn_personality_none_does_not_add_update_message() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let resp_mock = mount_sse_once(&server, sse_completed("resp-1")).await;
    let mut builder = test_chaos().with_model("gpt-5.2-codex");
    let test = builder.build(&server).await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: test.config.permissions.approval_policy.value(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: test.session_configured.model.clone(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    assert!(
        request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "hello"),
        "expected the request to include the user turn text"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_personality_some_sets_instructions_template() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let resp_mock = mount_sse_once(&server, sse_completed("resp-1")).await;
    let mut builder = test_chaos()
        .with_model("gpt-5.2-codex")
        .with_config(|config| {
            config.personality = Some(Personality::Friendly);
        });
    let test = builder.build(&server).await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: test.config.permissions.approval_policy.value(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: test.session_configured.model.clone(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    assert!(
        request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "hello"),
        "expected the request to include the user turn text"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_personality_none_sends_no_personality() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let resp_mock = mount_sse_once(&server, sse_completed("resp-1")).await;
    let mut builder = test_chaos()
        .with_model("gpt-5.2-codex")
        .with_config(|config| {
            config.personality = Some(Personality::None);
        });
    let test = builder.build(&server).await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: test.config.permissions.approval_policy.value(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: test.session_configured.model.clone(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    assert!(
        request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "hello"),
        "expected the request to include the user turn text"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_personality_is_pragmatic_without_config_toml() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let resp_mock = mount_sse_once(&server, sse_completed("resp-1")).await;
    let mut builder = test_chaos().with_model("gpt-5.2-codex");
    let test = builder.build(&server).await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: test.config.permissions.approval_policy.value(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: test.session_configured.model.clone(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    assert!(
        request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "hello"),
        "expected the request to include the user turn text"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_turn_personality_some_adds_update_message() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let resp_mock = mount_sse_sequence(
        &server,
        vec![sse_completed("resp-1"), sse_completed("resp-2")],
    )
    .await;
    let mut builder = test_chaos().with_model("exp-codex-personality");
    let test = builder.build(&server).await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: test.config.permissions.approval_policy.value(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: test.session_configured.model.clone(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.process
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,

            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: Some(Personality::Friendly),
        })
        .await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: test.config.permissions.approval_policy.value(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: test.session_configured.model.clone(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = resp_mock.requests();
    assert_eq!(requests.len(), 2, "expected two requests");
    assert!(
        requests.iter().all(|request| request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "hello")),
        "expected both turns to include the user turn text"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_turn_personality_same_value_does_not_add_update_message() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let resp_mock = mount_sse_sequence(
        &server,
        vec![sse_completed("resp-1"), sse_completed("resp-2")],
    )
    .await;
    let mut builder = test_chaos()
        .with_model("exp-codex-personality")
        .with_config(|config| {
            config.personality = Some(Personality::Pragmatic);
        });
    let test = builder.build(&server).await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: test.config.permissions.approval_policy.value(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: test.session_configured.model.clone(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.process
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,

            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: Some(Personality::Pragmatic),
        })
        .await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: test.config.permissions.approval_policy.value(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: test.session_configured.model.clone(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = resp_mock.requests();
    assert_eq!(requests.len(), 2, "expected two requests");
    assert!(
        requests.iter().all(|request| request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "hello")),
        "expected both turns to include the user turn text"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_model_friendly_personality_instructions_with_feature() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::builder()
        .body_print_limit(BodyPrintLimit::Limited(80_000))
        .start()
        .await;

    let remote_slug = "chaos-remote-default-personality";
    let mut remote_model = test_remote_model(remote_slug, ModelVisibility::List, 1);
    remote_model.display_name = "Remote default personality test".to_string();
    remote_model.description = Some("Remote model with default personality template".to_string());
    remote_model.shell_type = ConfigShellToolType::UnifiedExec;
    remote_model.model_messages = Some(ModelMessages {
        instructions_template: Some("Base instructions\n{{ personality }}\n".to_string()),
        instructions_variables: Some(ModelInstructionsVariables {
            personality_default: Some("Default from remote template".to_string()),
            personality_friendly: Some("Friendly variant".to_string()),
            personality_pragmatic: Some("Pragmatic variant".to_string()),
        }),
    });
    remote_model.context_window = Some(128_000);

    let _models_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model],
        },
    )
    .await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp-1")).await;

    let mut builder = test_chaos()
        .with_auth(chaos_kern::ChaosAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.model = Some(remote_slug.to_string());
            config.personality = Some(Personality::Friendly);
        });
    let test = builder.build(&server).await?;

    wait_for_model_available(&test.process_table.get_models_manager(), remote_slug).await;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: remote_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: Some(Personality::Friendly),
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    assert!(
        request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "hello"),
        "expected the request to include the user turn text"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_turn_personality_remote_model_template_includes_update_message() -> anyhow::Result<()>
{
    skip_if_no_network!(Ok(()));

    let server = MockServer::builder()
        .body_print_limit(BodyPrintLimit::Limited(80_000))
        .start()
        .await;

    let remote_slug = "chaos-remote-personality";
    let mut remote_model = test_remote_model(remote_slug, ModelVisibility::List, 1);
    remote_model.display_name = "Remote personality test".to_string();
    remote_model.description = Some("Remote model with personality template".to_string());
    remote_model.shell_type = ConfigShellToolType::UnifiedExec;
    remote_model.model_messages = Some(ModelMessages {
        instructions_template: Some("Base instructions\n{{ personality }}\n".to_string()),
        instructions_variables: Some(ModelInstructionsVariables {
            personality_default: None,
            personality_friendly: Some("Friendly from remote template".to_string()),
            personality_pragmatic: Some("Pragmatic from remote template".to_string()),
        }),
    });
    remote_model.context_window = Some(128_000);

    let _models_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model],
        },
    )
    .await;

    let resp_mock = mount_sse_sequence(
        &server,
        vec![sse_completed("resp-1"), sse_completed("resp-2")],
    )
    .await;

    let mut builder = test_chaos()
        .with_auth(chaos_kern::ChaosAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.model = Some("gpt-5.2-codex".to_string());
        });
    let test = builder.build(&server).await?;

    wait_for_model_available(&test.process_table.get_models_manager(), remote_slug).await;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: remote_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.process
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,

            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: Some(Personality::Friendly),
        })
        .await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: remote_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = resp_mock.requests();
    assert_eq!(requests.len(), 2, "expected two requests");
    assert!(
        requests.iter().all(|request| request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "hello")),
        "expected both turns to include the user turn text"
    );

    Ok(())
}
