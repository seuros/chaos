#![allow(clippy::unwrap_used)]

use anyhow::Context;
use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::user_input::UserInput;
use chaos_kern::shell::Shell;
use chaos_kern::shell::default_user_shell;
use chaos_realpath::AbsolutePathBuf;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::TestChaos;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use tempfile::TempDir;

fn text_user_input(text: String) -> serde_json::Value {
    text_user_input_parts(vec![text])
}

fn text_user_input_parts(texts: Vec<String>) -> serde_json::Value {
    serde_json::json!({
        "type": "message",
        "role": "user",
        "content": texts
            .into_iter()
            .map(|text| serde_json::json!({ "type": "input_text", "text": text }))
            .collect::<Vec<_>>()
    })
}

fn without_runtime_guidance(
    body1: serde_json::Value,
    body2: serde_json::Value,
) -> anyhow::Result<(serde_json::Value, serde_json::Value)> {
    let split = |mut body: serde_json::Value| -> anyhow::Result<_> {
        let input = body["input"].as_array_mut().context("input array")?;
        assert_eq!(
            input
                .iter()
                .filter(|item| {
                    item["role"] == "developer"
                        && item["content"][0]["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("# Tools"))
                })
                .count(),
            1,
            "expected exactly one request-local guidance message"
        );
        let guidance = input.pop().context("request-local runtime guidance")?;
        assert_eq!(guidance["role"], "developer");
        let content = guidance["content"].as_array().context("guidance content")?;
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "input_text");
        assert!(
            content[0]["text"]
                .as_str()
                .context("runtime guidance text")?
                .contains("# Tools"),
            "expected capability-aware runtime guidance: {guidance}"
        );
        Ok((body, guidance))
    };
    let (body1, guidance1) = split(body1)?;
    let (body2, guidance2) = split(body2)?;
    assert_eq!(
        guidance1, guidance2,
        "unchanged capabilities must produce identical request-local guidance"
    );
    // Runtime guidance is refreshed at the request tail, not persisted between
    // turns. Cache assertions below own the durable conversation prefix.
    Ok((body1, body2))
}

fn assert_default_env_context(text: &str, cwd: &str, shell: &Shell) {
    assert_env_context(text, cwd, shell);
    assert!(
        text.contains("<platform>") && text.contains("<arch>"),
        "expected platform in initial environment context: {text}"
    );
}

fn assert_env_context(text: &str, cwd: &str, shell: &Shell) {
    let shell_name = shell.name();
    assert!(
        text.starts_with(ENVIRONMENT_CONTEXT_OPEN_TAG),
        "expected environment context fragment: {text}"
    );
    assert!(
        text.contains(&format!("<cwd>{cwd}</cwd>")),
        "expected cwd in environment context: {text}"
    );
    assert!(
        text.contains(&format!("<shell name=\"{shell_name}\"")),
        "expected shell in environment context: {text}"
    );
    assert!(
        text.contains("<current_date>") && text.contains("</current_date>"),
        "expected current_date in environment context: {text}"
    );
    assert!(
        text.contains("<timezone>") && text.contains("</timezone>"),
        "expected timezone in environment context: {text}"
    );
    assert!(
        text.ends_with("</environment_context>"),
        "expected closing environment_context tag: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefixes_context_and_instructions_once_and_consistently_across_requests()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    use pretty_assertions::assert_eq;

    let server = start_mock_server().await;
    let req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;

    let TestChaos {
        home: _home,
        cwd: _cwd,
        process: chaos,
        config,
        ..
    } = test_chaos()
        .with_config(|config| {
            config.user_instructions = Some("be consistent and helpful".to_string());
        })
        .build(&server)
        .await?;

    chaos
        .submit(
            chaos
                .user_turn(
                    vec![UserInput::Text {
                        text: "hello 1".into(),
                        text_elements: Vec::new(),
                    }],
                    None,
                )
                .await,
        )
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    chaos
        .submit(
            chaos
                .user_turn(
                    vec![UserInput::Text {
                        text: "hello 2".into(),
                        text_elements: Vec::new(),
                    }],
                    None,
                )
                .await,
        )
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let (body1, body2) = without_runtime_guidance(
        req1.single_request().body_json(),
        req2.single_request().body_json(),
    )?;
    let input1 = body1["input"].as_array().expect("input array");
    assert_eq!(
        input1.len(),
        3,
        "expected permissions + cached contextual user prefix + user msg"
    );

    // `user_instructions` was evicted by b430498671 — it no longer rides
    // along in the contextual user prefix. Only the environment context
    // is bundled there now.
    let shell = default_user_shell();
    let cwd_str = config.cwd.to_string_lossy();
    let env_text = input1[1]["content"][0]["text"]
        .as_str()
        .expect("environment context text");
    assert_default_env_context(env_text, &cwd_str, &shell);
    assert_eq!(
        input1[1]["content"][0]["type"].as_str(),
        Some("input_text"),
        "expected environment context to be the sole cached contextual entry"
    );
    assert_eq!(
        input1[1]["content"].as_array().map(Vec::len),
        Some(1),
        "cached contextual user message should contain only environment context"
    );
    assert_eq!(input1[2], text_user_input("hello 1".to_string()));

    let input2 = body2["input"].as_array().expect("input array");
    assert_eq!(
        input2.len(),
        input1.len() + 1,
        "expected only the new user message after the cached prefix"
    );
    assert_eq!(
        &input2[..input1.len()],
        input1.as_slice(),
        "expected cached prefix to be reused"
    );
    assert_eq!(input2[input1.len()], text_user_input("hello 2".to_string()));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overrides_turn_context_but_keeps_cached_prefix_and_key_constant() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    use pretty_assertions::assert_eq;

    let server = start_mock_server().await;
    let req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;

    let TestChaos {
        process: chaos,
        home: _home,
        cwd: _cwd,
        ..
    } = test_chaos()
        .with_config(|config| {
            config.user_instructions = Some("be consistent and helpful".to_string());
        })
        .build(&server)
        .await?;

    // First turn
    chaos
        .submit(
            chaos
                .user_turn(
                    vec![UserInput::Text {
                        text: "hello 1".into(),
                        text_elements: Vec::new(),
                    }],
                    None,
                )
                .await,
        )
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let writable = TempDir::new().unwrap();
    let new_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![writable.path().try_into().unwrap()],
        read_only_access: Default::default(),
        network_access: true,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    chaos
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: Some(ApprovalPolicy::Headless),
            approvals_reviewer: None,
            sandbox_policy: Some(new_policy.clone()),

            model: None,
            effort: Some(Some(ReasoningEffort::High)),
            summary: Some(ReasoningSummary::Detailed),
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while chaos.config_snapshot().await.reasoning_effort != Some(ReasoningEffort::High) {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    chaos
        .submit(
            chaos
                .user_turn(
                    vec![UserInput::Text {
                        text: "hello 2".into(),
                        text_elements: Vec::new(),
                    }],
                    None,
                )
                .await,
        )
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request1 = req1.single_request();
    let request2 = req2.single_request();
    let (body1, body2) = without_runtime_guidance(request1.body_json(), request2.body_json())?;
    // prompt_cache_key should remain constant across overrides
    assert_eq!(
        body1["prompt_cache_key"], body2["prompt_cache_key"],
        "prompt_cache_key should not change across overrides"
    );

    // The entire prefix from the first request should be identical and reused
    // as the prefix of the second request, ensuring cache hit potential.
    let expected_user_message_2 = serde_json::json!({
        "type": "message",
        "role": "user",
        "content": [ { "type": "input_text", "text": "hello 2" } ]
    });
    let body1_input = body1["input"].as_array().expect("input array");
    let body2_input = body2["input"].as_array().expect("input array");
    let expected_permissions_msg = body1_input[0].clone();
    // After overriding the turn context, emit one updated permissions message.
    let expected_permissions_msg_2 = body2_input[body1_input.len()].clone();
    assert_ne!(
        expected_permissions_msg_2, expected_permissions_msg,
        "expected updated permissions message after override"
    );
    let mut expected_body2 = body1_input.to_vec();
    expected_body2.push(expected_permissions_msg_2);
    expected_body2.push(expected_user_message_2);
    assert_eq!(body2_input, expected_body2.as_slice());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_turn_overrides_keep_cached_prefix_and_key_constant() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    use pretty_assertions::assert_eq;

    let server = start_mock_server().await;
    let req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;

    let TestChaos {
        process: chaos,
        home: _home,
        cwd: _cwd,
        ..
    } = test_chaos()
        .with_config(|config| {
            config.user_instructions = Some("be consistent and helpful".to_string());
        })
        .build(&server)
        .await?;

    // First turn
    chaos
        .submit(
            chaos
                .user_turn(
                    vec![UserInput::Text {
                        text: "hello 1".into(),
                        text_elements: Vec::new(),
                    }],
                    None,
                )
                .await,
        )
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Second turn using per-turn overrides via UserTurn
    let new_cwd = TempDir::new().unwrap();
    let writable = TempDir::new().unwrap();
    let new_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![AbsolutePathBuf::try_from(writable.path()).unwrap()],
        read_only_access: Default::default(),
        network_access: true,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    chaos
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello 2".into(),
                text_elements: Vec::new(),
            }],
            cwd: new_cwd.path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            vfs_policy: chaos_ipc::permissions::VfsPolicy::from_sandbox_policy(
                &new_policy,
                new_cwd.path(),
            ),
            socket_policy: (&new_policy).into(),
            model: "o3".to_string(),
            effort: Some(ReasoningEffort::High),
            summary: Some(ReasoningSummary::Detailed),
            service_tier: None,
            collaboration_mode: None,
            final_output_json_schema: None,
            personality: None,
        })
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request1 = req1.single_request();
    let request2 = req2.single_request();
    let (body1, body2) = without_runtime_guidance(request1.body_json(), request2.body_json())?;

    // prompt_cache_key should remain constant across per-turn overrides
    assert_eq!(
        body1["prompt_cache_key"], body2["prompt_cache_key"],
        "prompt_cache_key should not change across per-turn overrides"
    );

    // The entire prefix from the first request should be identical and reused
    // as the prefix of the second request.
    let expected_user_message_2 = serde_json::json!({
        "type": "message",
        "role": "user",
        "content": [ { "type": "input_text", "text": "hello 2" } ]
    });
    let body1_input = body1["input"].as_array().expect("input array");
    let body2_input = body2["input"].as_array().expect("input array");
    let expected_permissions_msg = body1_input[0].clone();
    let expected_settings_update_msg = body2_input[body1_input.len()].clone();
    assert_ne!(
        expected_settings_update_msg, expected_permissions_msg,
        "expected updated permissions message after per-turn override"
    );
    assert_eq!(
        expected_settings_update_msg["role"].as_str(),
        Some("developer")
    );
    assert!(
        request2.has_message_with_input_texts("developer", |texts| {
            texts.iter().any(|text| text.contains("<model_switch>"))
        }),
        "expected model switch section after model override: {expected_settings_update_msg:?}"
    );
    let expected_env_msg_2 = body2_input[body1_input.len() + 1].clone();
    assert_eq!(expected_env_msg_2["role"].as_str(), Some("user"));
    let env_text = expected_env_msg_2["content"][0]["text"]
        .as_str()
        .expect("environment context text");
    let shell = default_user_shell();
    let expected_cwd = new_cwd.path().display().to_string();
    assert_env_context(env_text, &expected_cwd, &shell);
    assert!(
        !env_text.contains("<platform>"),
        "per-turn updates must not repeat the cached platform: {env_text}"
    );
    let mut expected_body2 = body1_input.to_vec();
    expected_body2.push(expected_settings_update_msg);
    expected_body2.push(expected_env_msg_2);
    expected_body2.push(expected_user_message_2);
    assert_eq!(body2_input, expected_body2.as_slice());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_user_turn_with_no_changes_does_not_send_environment_context() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    use pretty_assertions::assert_eq;

    let server = start_mock_server().await;
    let req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;

    let TestChaos {
        home: _home,
        cwd: _cwd,
        process: chaos,
        config,
        session_configured,
        ..
    } = test_chaos()
        .with_config(|config| {
            config.user_instructions = Some("be consistent and helpful".to_string());
        })
        .build(&server)
        .await?;

    let default_cwd = config.cwd.clone();
    let default_approval_policy = config.permissions.approval_policy.value();
    let default_model = session_configured.model;
    let default_effort = config.model_reasoning_effort;
    let default_summary = config.model_reasoning_summary;

    chaos
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello 1".into(),
                text_elements: Vec::new(),
            }],
            cwd: default_cwd.clone(),
            approval_policy: default_approval_policy,
            vfs_policy: config.permissions.vfs_policy.clone(),
            socket_policy: config.permissions.socket_policy,
            model: default_model.clone(),
            effort: default_effort,
            summary: Some(default_summary.unwrap_or(ReasoningSummary::Auto)),
            service_tier: None,
            collaboration_mode: None,
            final_output_json_schema: None,
            personality: None,
        })
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    chaos
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello 2".into(),
                text_elements: Vec::new(),
            }],
            cwd: default_cwd.clone(),
            approval_policy: default_approval_policy,
            vfs_policy: config.permissions.vfs_policy.clone(),
            socket_policy: config.permissions.socket_policy,
            model: default_model.clone(),
            effort: default_effort,
            summary: Some(default_summary.unwrap_or(ReasoningSummary::Auto)),
            service_tier: None,
            collaboration_mode: None,
            final_output_json_schema: None,
            personality: None,
        })
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request1 = req1.single_request();
    let request2 = req2.single_request();
    let (body1, body2) = without_runtime_guidance(request1.body_json(), request2.body_json())?;

    let body1_input = body1["input"].as_array().expect("input array");
    let body2_input = body2["input"].as_array().expect("input array");
    let expected_permissions_msg = body1_input[0].clone();
    let expected_ui_msg = body1_input[1].clone();

    // Post b430498671: `user_instructions` no longer rides along in the
    // cached contextual user prefix. Only the environment context is
    // bundled there.
    let shell = default_user_shell();
    let default_cwd_lossy = default_cwd.to_string_lossy();
    let expected_env_text_1 = expected_ui_msg["content"][0]["text"]
        .as_str()
        .expect("cached environment context text")
        .to_string();
    assert_default_env_context(&expected_env_text_1, &default_cwd_lossy, &shell);

    let expected_contextual_user_msg_1 = text_user_input_parts(vec![expected_env_text_1]);
    let expected_user_message_1 = text_user_input("hello 1".to_string());

    let expected_input_1 = vec![
        expected_permissions_msg.clone(),
        expected_contextual_user_msg_1.clone(),
        expected_user_message_1.clone(),
    ];
    assert_eq!(body1_input, expected_input_1.as_slice());

    let expected_user_message_2 = text_user_input("hello 2".to_string());
    let expected_input_2 = vec![
        expected_permissions_msg,
        expected_contextual_user_msg_1,
        expected_user_message_1,
        expected_user_message_2,
    ];
    assert_eq!(body2_input, expected_input_2.as_slice());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_user_turn_with_changes_sends_environment_context() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    use pretty_assertions::assert_eq;

    let server = start_mock_server().await;

    let req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;
    let TestChaos {
        home: _home,
        cwd: _cwd,
        process: chaos,
        config,
        session_configured,
        ..
    } = test_chaos()
        .with_config(|config| {
            config.user_instructions = Some("be consistent and helpful".to_string());
        })
        .build(&server)
        .await?;

    let default_cwd = config.cwd.clone();
    let default_approval_policy = config.permissions.approval_policy.value();
    let default_model = session_configured.model;
    let default_effort = config.model_reasoning_effort;
    let default_summary = config.model_reasoning_summary;

    chaos
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello 1".into(),
                text_elements: Vec::new(),
            }],
            cwd: default_cwd.clone(),
            approval_policy: default_approval_policy,
            vfs_policy: config.permissions.vfs_policy.clone(),
            socket_policy: config.permissions.socket_policy,
            model: default_model,
            effort: default_effort,
            summary: Some(default_summary.unwrap_or(ReasoningSummary::Auto)),
            service_tier: None,
            collaboration_mode: None,
            final_output_json_schema: None,
            personality: None,
        })
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    chaos
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello 2".into(),
                text_elements: Vec::new(),
            }],
            cwd: default_cwd.clone(),
            approval_policy: ApprovalPolicy::Headless,
            vfs_policy: (&SandboxPolicy::RootAccess).into(),
            socket_policy: (&SandboxPolicy::RootAccess).into(),
            model: "o3".to_string(),
            effort: Some(ReasoningEffort::High),
            summary: Some(ReasoningSummary::Detailed),
            service_tier: None,
            collaboration_mode: None,
            final_output_json_schema: None,
            personality: None,
        })
        .await?;
    wait_for_event(&chaos, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request1 = req1.single_request();
    let request2 = req2.single_request();
    let (body1, body2) = without_runtime_guidance(request1.body_json(), request2.body_json())?;

    let body1_input = body1["input"].as_array().expect("input array");
    let body2_input = body2["input"].as_array().expect("input array");
    let expected_permissions_msg = body1_input[0].clone();
    let expected_ui_msg = body1_input[1].clone();

    // Post b430498671: only environment context is bundled into the
    // cached contextual user prefix; `user_instructions` was evicted.
    let shell = default_user_shell();
    let expected_env_text_1 = expected_ui_msg["content"][0]["text"]
        .as_str()
        .expect("cached environment context text")
        .to_string();
    assert_default_env_context(&expected_env_text_1, &default_cwd.to_string_lossy(), &shell);
    let expected_contextual_user_msg_1 = text_user_input_parts(vec![expected_env_text_1]);
    let expected_user_message_1 = text_user_input("hello 1".to_string());
    let expected_input_1 = vec![
        expected_permissions_msg.clone(),
        expected_contextual_user_msg_1.clone(),
        expected_user_message_1.clone(),
    ];
    assert_eq!(body1_input, expected_input_1.as_slice());

    let expected_settings_update_msg = body2_input[body1_input.len()].clone();
    assert_ne!(
        expected_settings_update_msg, expected_permissions_msg,
        "expected updated permissions message after policy change"
    );
    assert_eq!(
        expected_settings_update_msg["role"].as_str(),
        Some("developer")
    );
    assert!(
        request2.has_message_with_input_texts("developer", |texts| {
            texts.iter().any(|text| text.contains("<model_switch>"))
        }),
        "expected model switch section after model override: {expected_settings_update_msg:?}"
    );
    let expected_user_message_2 = text_user_input("hello 2".to_string());
    let expected_input_2 = vec![
        expected_permissions_msg,
        expected_contextual_user_msg_1,
        expected_user_message_1,
        expected_settings_update_msg,
        expected_user_message_2,
    ];
    assert_eq!(body2_input, expected_input_2.as_slice());

    Ok(())
}
