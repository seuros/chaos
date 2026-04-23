use anyhow::Result;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::openai_models::InputModality;
use chaos_ipc::openai_models::ModelsResponse;
use chaos_ipc::openai_models::default_input_modalities;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::user_input::UserInput;
use chaos_kern::ChaosAuth;
use chaos_kern::config::types::Personality;
use chaos_kern::models_manager::manager::RefreshStrategy;
use chaos_kern::test_support::test_model_info_with_input_modalities as test_model_info;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_image_generation_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_models_once;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_completed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use wiremock::MockServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_change_appends_model_instructions_developer_message() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    let resp_mock = mount_sse_sequence(
        &server,
        vec![sse_completed("resp-1"), sse_completed("resp-2")],
    )
    .await;

    let mut builder = test_chaos().with_model("gpt-5.2-codex");
    let test = builder.build(&server).await?;
    let next_model = "gpt-5.1-codex-max";

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

            model: Some(next_model.to_string()),
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "switch models".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: next_model.to_string(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = resp_mock.requests();
    assert_eq!(requests.len(), 2, "expected two model requests");

    let second_request = requests.last().expect("expected second request");
    let developer_texts = second_request.message_input_texts("developer");
    assert!(
        !developer_texts.is_empty(),
        "expected a developer update when the model changes"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_and_personality_change_only_appends_model_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let resp_mock = mount_sse_sequence(
        &server,
        vec![sse_completed("resp-1"), sse_completed("resp-2")],
    )
    .await;

    let mut builder = test_chaos().with_model("gpt-5.2-codex");
    let test = builder.build(&server).await?;
    let next_model = "exp-codex-personality";

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

            model: Some(next_model.to_string()),
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
                text: "switch model and personality".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: next_model.to_string(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = resp_mock.requests();
    assert_eq!(requests.len(), 2, "expected two model requests");

    let second_request = requests.last().expect("expected second request");
    let developer_texts = second_request.message_input_texts("developer");
    assert!(
        !developer_texts.is_empty(),
        "expected a developer update when the model changes"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_tier_change_is_applied_on_next_http_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let resp_mock = mount_sse_sequence(
        &server,
        vec![sse_completed("resp-1"), sse_completed("resp-2")],
    )
    .await;

    let test = test_chaos().build(&server).await?;

    test.submit_turn_with_service_tier("fast turn", Some(ServiceTier::Fast))
        .await?;
    test.submit_turn_with_service_tier("standard turn", None)
        .await?;

    let requests = resp_mock.requests();
    assert_eq!(requests.len(), 2, "expected two model requests");

    let first_body = requests[0].body_json();
    let second_body = requests[1].body_json();

    assert_eq!(first_body["service_tier"].as_str(), Some("priority"));
    assert_eq!(second_body.get("service_tier"), None);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flex_service_tier_is_applied_to_http_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let resp_mock = mount_sse_once(&server, sse_completed("resp-1")).await;

    let test = test_chaos().build(&server).await?;

    test.submit_turn_with_service_tier("flex turn", Some(ServiceTier::Flex))
        .await?;

    let request = resp_mock.single_request();
    let body = request.body_json();
    assert_eq!(body["service_tier"].as_str(), Some("flex"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_change_from_image_to_text_strips_prior_image_content() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    let image_model_slug = "test-image-model";
    let text_model_slug = "test-text-only-model";
    let image_model = test_model_info(
        image_model_slug,
        "Test Image Model",
        "supports image input",
        default_input_modalities(),
    );
    let text_model = test_model_info(
        text_model_slug,
        "Test Text Model",
        "text only",
        vec![InputModality::Text],
    );
    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![image_model, text_model],
        },
    )
    .await;

    let responses = mount_sse_sequence(
        &server,
        vec![sse_completed("resp-1"), sse_completed("resp-2")],
    )
    .await;

    let mut builder = test_chaos()
        .with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.model = Some(image_model_slug.to_string());
        });
    let test = builder.build(&server).await?;
    let models_manager = test.process_table.get_models_manager();
    let _ = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;
    let image_url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR4nGNgYAAAAAMAASsJTYQAAAAASUVORK5CYII="
        .to_string();

    test.process
        .submit(Op::UserTurn {
            items: vec![
                UserInput::Image {
                    image_url: image_url.clone(),
                },
                UserInput::Text {
                    text: "first turn".to_string(),
                    text_elements: Vec::new(),
                },
            ],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: image_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "second turn".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: text_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2, "expected two model requests");

    let first_request = requests.first().expect("expected first request");
    assert!(
        !first_request.message_input_image_urls("user").is_empty(),
        "first request should include the uploaded image"
    );

    let second_request = requests.last().expect("expected second request");
    assert!(
        second_request.message_input_image_urls("user").is_empty(),
        "second request should strip unsupported image content"
    );
    let second_user_texts = second_request.message_input_texts("user");
    assert!(
        second_user_texts.iter().any(|text| text == "first turn"),
        "second request should preserve the original user text"
    );
    assert!(
        second_user_texts.len() > 1,
        "second request should preserve a text placeholder for the stripped image history"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_image_is_replayed_for_image_capable_models() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let saved_path = std::env::temp_dir().join("ig_123.png");
    let _ = std::fs::remove_file(&saved_path);

    let server = MockServer::start().await;
    let image_model_slug = "test-image-model";
    let image_model = test_model_info(
        image_model_slug,
        "Test Image Model",
        "supports image input",
        default_input_modalities(),
    );
    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![image_model],
        },
    )
    .await;

    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_image_generation_call("ig_123", "completed", "lobster", "Zm9v"),
                ev_completed_with_tokens("resp-1", 10),
            ]),
            sse_completed("resp-2"),
        ],
    )
    .await;

    let mut builder = test_chaos()
        .with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.model = Some(image_model_slug.to_string());
        });
    let test = builder.build(&server).await?;
    let models_manager = test.process_table.get_models_manager();
    let _ = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "generate a lobster".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: image_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            service_tier: None,
            summary: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "describe the generated image".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: image_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            service_tier: None,
            summary: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2, "expected two model requests");

    let second_request = requests.last().expect("expected second request");
    let image_generation_calls = second_request.inputs_of_type("image_generation_call");
    assert_eq!(
        image_generation_calls.len(),
        1,
        "expected generated image history to be replayed as an image_generation_call"
    );
    assert_eq!(
        image_generation_calls[0]["id"].as_str(),
        Some("ig_123"),
        "expected the original image generation call id to be preserved"
    );
    assert_eq!(
        image_generation_calls[0]["result"].as_str(),
        Some("Zm9v"),
        "expected the original generated image payload to be preserved"
    );
    assert!(
        !second_request.message_input_texts("developer").is_empty(),
        "second request should retain developer-visible context for the saved image"
    );
    let _ = std::fs::remove_file(&saved_path);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_change_from_generated_image_to_text_preserves_prior_generated_image_call()
-> Result<()> {
    skip_if_no_network!(Ok(()));

    let saved_path = std::env::temp_dir().join("ig_123.png");
    let _ = std::fs::remove_file(&saved_path);

    let server = MockServer::start().await;
    let image_model_slug = "test-image-model";
    let text_model_slug = "test-text-only-model";
    let image_model = test_model_info(
        image_model_slug,
        "Test Image Model",
        "supports image input",
        default_input_modalities(),
    );
    let text_model = test_model_info(
        text_model_slug,
        "Test Text Model",
        "text only",
        vec![InputModality::Text],
    );
    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![image_model, text_model],
        },
    )
    .await;

    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_image_generation_call("ig_123", "completed", "lobster", "Zm9v"),
                ev_completed_with_tokens("resp-1", 10),
            ]),
            sse_completed("resp-2"),
        ],
    )
    .await;

    let mut builder = test_chaos()
        .with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.model = Some(image_model_slug.to_string());
        });
    let test = builder.build(&server).await?;
    let models_manager = test.process_table.get_models_manager();
    let _ = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "generate a lobster".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: image_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            service_tier: None,
            summary: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "describe the generated image".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: text_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            service_tier: None,
            summary: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2, "expected two model requests");

    let second_request = requests.last().expect("expected second request");
    let image_generation_calls = second_request.inputs_of_type("image_generation_call");
    assert!(
        second_request.message_input_image_urls("user").is_empty(),
        "second request should not rewrite generated images into message input images"
    );
    assert!(
        image_generation_calls.len() == 1,
        "second request should preserve the generated image call for text-only models"
    );
    assert_eq!(
        image_generation_calls[0]["id"].as_str(),
        Some("ig_123"),
        "second request should preserve the original generated image call id"
    );
    assert_eq!(
        image_generation_calls[0]["result"].as_str(),
        Some(""),
        "second request should strip generated image bytes for text-only models"
    );
    assert!(
        second_request
            .message_input_texts("user")
            .iter()
            .all(|text| !text.contains("do not support image input")),
        "second request should not inject the unsupported-image placeholder text"
    );
    assert!(
        !second_request.message_input_texts("developer").is_empty(),
        "second request should retain developer-visible context for the generated image"
    );
    let _ = std::fs::remove_file(&saved_path);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_rollback_after_generated_image_drops_entire_image_turn_history() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let saved_path = std::env::temp_dir().join("ig_rollback.png");
    let _ = std::fs::remove_file(&saved_path);

    let server = MockServer::start().await;
    let image_model_slug = "test-image-model";
    let image_model = test_model_info(
        image_model_slug,
        "Test Image Model",
        "supports image input",
        default_input_modalities(),
    );
    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![image_model],
        },
    )
    .await;

    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_image_generation_call("ig_rollback", "completed", "lobster", "Zm9v"),
                ev_completed_with_tokens("resp-1", 10),
            ]),
            sse_completed("resp-2"),
        ],
    )
    .await;

    let mut builder = test_chaos()
        .with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.model = Some(image_model_slug.to_string());
        });
    let test = builder.build(&server).await?;
    let models_manager = test.process_table.get_models_manager();
    let _ = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "generate a lobster".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: image_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            service_tier: None,
            summary: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.process
        .submit(Op::ProcessRollback { num_turns: 1 })
        .await?;
    wait_for_event(&test.process, |ev| {
        matches!(ev, EventMsg::ProcessRolledBack(_))
    })
    .await;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "after rollback".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: image_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            service_tier: None,
            summary: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2, "expected two model requests");

    let second_request = requests.last().expect("expected second request");
    assert!(
        !second_request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "generate a lobster"),
        "rollback should remove the rolled-back image-generation user turn"
    );
    assert!(
        second_request
            .inputs_of_type("image_generation_call")
            .is_empty(),
        "rollback should remove the generated image call with the rolled-back turn"
    );
    let _ = std::fs::remove_file(&saved_path);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_switch_to_smaller_model_updates_token_context_window() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let large_model_slug = "test-image-model";
    let smaller_model_slug = "test-text-only-model";
    let large_context_window = 272_000;
    let smaller_context_window = 128_000;
    let effective_context_window_percent = 95;
    let large_effective_window = (large_context_window * effective_context_window_percent) / 100;
    let smaller_effective_window =
        (smaller_context_window * effective_context_window_percent) / 100;

    let mut base_model = test_model_info(
        large_model_slug,
        "Larger Model",
        "larger context window model",
        default_input_modalities(),
    );
    base_model.context_window = Some(large_context_window);
    base_model.effective_context_window_percent = effective_context_window_percent;
    let mut smaller_model = base_model.clone();
    smaller_model.slug = smaller_model_slug.to_string();
    smaller_model.display_name = "Smaller Model".to_string();
    smaller_model.description = Some("smaller context window model".to_string());
    smaller_model.context_window = Some(smaller_context_window);

    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![base_model, smaller_model],
        },
    )
    .await;

    mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_completed_with_tokens("resp-1", 100),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_completed_with_tokens("resp-2", 120),
            ]),
        ],
    )
    .await;

    let mut builder = test_chaos()
        .with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.model = Some(large_model_slug.to_string());
        });
    let test = builder.build(&server).await?;

    let models_manager = test.process_table.get_models_manager();
    let available_models = models_manager.list_models(RefreshStrategy::Online).await;
    assert!(
        available_models
            .iter()
            .any(|model| model.model == smaller_model_slug),
        "expected {smaller_model_slug} to be available in remote model list"
    );
    let large_model_info = models_manager
        .get_model_info(large_model_slug, &test.config)
        .await;
    assert_eq!(large_model_info.context_window, Some(large_context_window));
    let smaller_model_info = models_manager
        .get_model_info(smaller_model_slug, &test.config)
        .await;
    assert_eq!(
        smaller_model_info.context_window,
        Some(smaller_context_window)
    );

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "use larger model".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: large_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let large_window_event = wait_for_event(&test.process, |event| {
        matches!(
            event,
            EventMsg::TokenCount(token_count)
                if token_count
                    .info
                    .as_ref()
                    .is_some_and(|info| info.last_token_usage.total_tokens == 100)
        )
    })
    .await;
    let EventMsg::TokenCount(large_token_count) = large_window_event else {
        unreachable!("wait_for_event returned unexpected event");
    };
    assert_eq!(
        large_token_count
            .info
            .as_ref()
            .and_then(|info| info.model_context_window),
        Some(large_effective_window)
    );
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.process
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,

            model: Some(smaller_model_slug.to_string()),
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "switch to smaller model".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            model: smaller_model_slug.to_string(),
            effort: test.config.model_reasoning_effort,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let smaller_turn_started_event = wait_for_event(&test.process, |event| {
        matches!(
            event,
            EventMsg::TurnStarted(started)
                if started.model_context_window == Some(smaller_effective_window)
        )
    })
    .await;
    let EventMsg::TurnStarted(smaller_turn_started) = smaller_turn_started_event else {
        unreachable!("wait_for_event returned unexpected event");
    };
    assert_eq!(
        smaller_turn_started.model_context_window,
        Some(smaller_effective_window)
    );

    let smaller_window_event = wait_for_event(&test.process, |event| {
        matches!(
            event,
            EventMsg::TokenCount(token_count)
                if token_count
                    .info
                    .as_ref()
                    .is_some_and(|info| info.last_token_usage.total_tokens == 120)
        )
    })
    .await;
    let EventMsg::TokenCount(smaller_token_count) = smaller_window_event else {
        unreachable!("wait_for_event returned unexpected event");
    };
    let smaller_window = smaller_token_count
        .info
        .as_ref()
        .and_then(|info| info.model_context_window);
    assert_eq!(smaller_window, Some(smaller_effective_window));
    assert_ne!(smaller_window, Some(large_effective_window));
    wait_for_event(&test.process, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    Ok(())
}
