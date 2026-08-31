use super::*;

#[tokio::test]
async fn session_configuration_apply_preserves_split_file_system_policy_on_cwd_only_update() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let workspace = tempfile::tempdir().expect("create temp dir");
    let project_root = workspace.path().join("project");
    let original_cwd = project_root.join("subdir");
    let docs_dir = original_cwd.join("docs");
    std::fs::create_dir_all(&docs_dir).expect("create docs dir");
    let docs_dir = chaos_realpath::AbsolutePathBuf::from_absolute_path(&docs_dir).expect("docs");

    session_configuration.cwd = original_cwd;
    session_configuration.vfs_policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::CurrentWorkingDirectory,
            },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: docs_dir },
            access: VfsAccessMode::Read,
        },
    ]);

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(project_root),
            ..Default::default()
        })
        .expect("cwd-only update should succeed");

    assert_eq!(updated.vfs_policy, session_configuration.vfs_policy);
}

#[tokio::test]
async fn session_configuration_apply_rederives_projected_file_system_policy_on_cwd_update() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let workspace = tempfile::tempdir().expect("create temp dir");
    let project_root = workspace.path().join("project");
    let original_cwd = project_root.join("subdir");
    let docs_dir = original_cwd.join("docs");
    std::fs::create_dir_all(&docs_dir).expect("create docs dir");
    let docs_dir = chaos_realpath::AbsolutePathBuf::from_absolute_path(&docs_dir).expect("docs");

    session_configuration.cwd = original_cwd;
    let sandbox_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: Vec::new(),
        read_only_access: ReadOnlyAccess::Restricted {
            include_platform_defaults: true,
            readable_roots: vec![docs_dir],
        },
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(project_root.clone()),
            sandbox_policy: Some(sandbox_policy.clone()),
            ..Default::default()
        })
        .expect("sandbox update should succeed");

    assert_eq!(
        updated.vfs_policy,
        VfsPolicy::from_sandbox_policy(&sandbox_policy, &project_root,)
    );
}

#[tokio::test]
async fn legacy_collaboration_mode_updates_keep_mode_policy_in_sync() {
    let session_configuration = make_session_configuration_for_tests().await;
    let plan_mode = session_configuration
        .mode_registry
        .apply_mode(
            crate::modes::PLAN_MODE_ID,
            &session_configuration.collaboration_mode,
        )
        .expect("build plan collaboration mode");

    let plan_configuration = session_configuration
        .apply(&SessionSettingsUpdate {
            collaboration_mode: Some(plan_mode),
            ..Default::default()
        })
        .expect("apply plan mode");
    assert_eq!(
        plan_configuration.mode_policy.active_mode,
        crate::modes::PLAN_MODE_ID
    );
    assert_eq!(
        plan_configuration.collaboration_mode.reasoning_effort(),
        Some(chaos_ipc::openai_models::ReasoningEffort::Medium)
    );

    let default_mode = plan_configuration
        .mode_registry
        .apply_mode(
            crate::modes::DEFAULT_MODE_ID,
            &plan_configuration.collaboration_mode,
        )
        .expect("build default collaboration mode");
    let default_configuration = plan_configuration
        .apply(&SessionSettingsUpdate {
            collaboration_mode: Some(default_mode),
            ..Default::default()
        })
        .expect("apply default mode");
    assert_eq!(
        default_configuration.mode_policy.active_mode,
        crate::modes::DEFAULT_MODE_ID
    );
    assert_eq!(
        default_configuration.collaboration_mode.reasoning_effort(),
        session_configuration.mode_base_reasoning_effort
    );
}

#[tokio::test]
async fn legacy_collaboration_mode_aliases_use_the_current_live_mode() {
    let session_configuration = make_session_configuration_for_tests().await;

    for legacy_mode in [
        chaos_ipc::config_types::ModeKind::Execute,
        chaos_ipc::config_types::ModeKind::PairProgramming,
    ] {
        let updated = session_configuration
            .apply(&SessionSettingsUpdate {
                collaboration_mode: Some(chaos_ipc::config_types::CollaborationMode {
                    mode: legacy_mode,
                    settings: chaos_ipc::config_types::Settings {
                        model: session_configuration.collaboration_mode.model().to_string(),
                        reasoning_effort: None,
                        minion_instructions: None,
                    },
                }),
                ..Default::default()
            })
            .expect("apply legacy collaboration mode alias");

        assert_eq!(
            updated.mode_policy.active_mode,
            crate::modes::DEFAULT_MODE_ID
        );
        assert_eq!(
            updated.collaboration_mode.mode,
            chaos_ipc::config_types::ModeKind::Default
        );
    }
}

#[tokio::test]
async fn switch_mode_changes_the_next_sample_context_in_the_same_session() {
    let (session, turn_context) = make_session_and_context().await;
    let turn_context = Arc::new(turn_context);
    let base_reasoning_effort = turn_context.reasoning_effort;

    let result = session
        .switch_mode(crate::modes::PLAN_MODE_ID, turn_context.as_ref())
        .await
        .expect("switch to plan");
    assert!(result.changed);
    assert_eq!(turn_context.mode_id, crate::modes::DEFAULT_MODE_ID);

    let plan_context = session.effective_turn_context(&turn_context).await;
    assert_eq!(plan_context.mode_id, crate::modes::PLAN_MODE_ID);
    assert!(!plan_context.mode_capabilities.mutation);
    assert!(plan_context.tools_config.apply_patch_tool_type.is_none());
    assert!(!plan_context.tools_config.mode_allow_update_plan);
    assert!(plan_context.tools_config.mode_switching);

    session
        .switch_mode(crate::modes::DEFAULT_MODE_ID, plan_context.as_ref())
        .await
        .expect("switch back to default");
    let default_context = session.effective_turn_context(&turn_context).await;
    assert_eq!(default_context.mode_id, crate::modes::DEFAULT_MODE_ID);
    assert!(default_context.mode_capabilities.mutation);
    assert!(default_context.tools_config.mode_allow_update_plan);
    assert_eq!(default_context.reasoning_effort, base_reasoning_effort);
}

#[tokio::test]
async fn switch_mode_emits_ui_sync_only_for_main_sessions() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;

    session
        .switch_mode(crate::modes::PLAN_MODE_ID, turn_context.as_ref())
        .await
        .expect("switch main session to plan");
    let event = loop {
        let event = tokio::time::timeout(StdDuration::from_secs(1), rx.recv())
            .await
            .expect("mode change event timed out")
            .expect("mode change event missing");
        if let EventMsg::SessionModeChanged(event) = event.msg {
            break event;
        }
    };
    assert_eq!(event.session_id, session.conversation_id);
    assert_eq!(event.mode_id, crate::modes::PLAN_MODE_ID);
    assert_eq!(event.mode_title, "Plan");
    assert_eq!(event.mode_kind, chaos_ipc::config_types::ModeKind::Plan);
    assert_eq!(
        event.reasoning_effort,
        Some(chaos_ipc::openai_models::ReasoningEffort::Medium)
    );

    session
        .switch_mode(crate::modes::PLAN_MODE_ID, turn_context.as_ref())
        .await
        .expect("repeat active mode");
    assert_no_session_mode_changed(&rx).await;

    let (minion, minion_turn, minion_rx) = make_session_and_context_with_rx().await;
    {
        let mut state = minion.state.lock().await;
        state.session_configuration.session_source = crate::protocol::SessionSource::SubAgent(
            crate::protocol::SubAgentSource::Other("test-minion".to_string()),
        );
    }
    minion
        .switch_mode(crate::modes::PLAN_MODE_ID, minion_turn.as_ref())
        .await
        .expect("switch minion to plan");
    assert_no_session_mode_changed(&minion_rx).await;
}

async fn assert_no_session_mode_changed(rx: &async_channel::Receiver<Event>) {
    let deadline = std::time::Instant::now() + StdDuration::from_millis(100);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Err(_) => return,
            Ok(Err(_)) => return,
            Ok(Ok(event)) => assert!(
                !matches!(event.msg, EventMsg::SessionModeChanged(_)),
                "unexpected session_mode_changed event"
            ),
        }
    }
}

#[tokio::test]
async fn switch_mode_is_scoped_to_one_session() {
    let (first_session, first_turn) = make_session_and_context().await;
    let (second_session, second_turn) = make_session_and_context().await;
    let first_turn = Arc::new(first_turn);
    let second_turn = Arc::new(second_turn);

    first_session
        .switch_mode(crate::modes::PLAN_MODE_ID, first_turn.as_ref())
        .await
        .expect("switch first session");

    let first_effective = first_session.effective_turn_context(&first_turn).await;
    let second_effective = second_session.effective_turn_context(&second_turn).await;
    assert_eq!(first_effective.mode_id, crate::modes::PLAN_MODE_ID);
    assert_eq!(second_effective.mode_id, crate::modes::DEFAULT_MODE_ID);

    let second_resource = second_session.modes_json().await.expect("modes resource");
    let second_resource: serde_json::Value =
        serde_json::from_str(&second_resource).expect("parse modes resource");
    assert_eq!(
        second_resource["active_mode"],
        crate::modes::DEFAULT_MODE_ID
    );
}

// todo: use online model info

#[tokio::test]
async fn notify_request_permissions_response_ignores_unmatched_call_id() {
    let (session, _turn_context) = make_session_and_context().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());

    session
        .notify_request_permissions_response(
            "missing",
            chaos_ipc::request_permissions::RequestPermissionsResponse {
                permissions: RequestPermissionProfile {
                    network: Some(chaos_ipc::models::NetworkPermissions {
                        enabled: Some(true),
                    }),
                    ..RequestPermissionProfile::default()
                },
                scope: PermissionGrantScope::Turn,
            },
        )
        .await;

    assert_eq!(session.granted_turn_permissions().await, None);
}

#[tokio::test]
async fn request_permissions_emits_event_when_granular_policy_allows_requests() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(crate::protocol::ApprovalPolicy::Granular(
            crate::protocol::GranularApprovalConfig {
                sandbox_approval: true,
                rules: true,
                request_permissions: true,
                mcp_elicitations: true,
            },
        ))
        .expect("test setup should allow updating approval policy");

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "call-1".to_string();
    let expected_response = chaos_ipc::request_permissions::RequestPermissionsResponse {
        permissions: RequestPermissionProfile {
            network: Some(chaos_ipc::models::NetworkPermissions {
                enabled: Some(true),
            }),
            ..RequestPermissionProfile::default()
        },
        scope: PermissionGrantScope::Turn,
    };

    let handle = tokio::spawn({
        let session = Arc::clone(&session);
        let turn_context = Arc::clone(&turn_context);
        let call_id = call_id.clone();
        async move {
            session
                .request_permissions(
                    turn_context.as_ref(),
                    call_id,
                    chaos_ipc::request_permissions::RequestPermissionsArgs {
                        reason: Some("need network".to_string()),
                        permissions: RequestPermissionProfile {
                            network: Some(chaos_ipc::models::NetworkPermissions {
                                enabled: Some(true),
                            }),
                            ..RequestPermissionProfile::default()
                        },
                    },
                )
                .await
        }
    });

    let request_event = tokio::time::timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("request_permissions event timed out")
        .expect("request_permissions event missing");
    let EventMsg::RequestPermissions(request) = request_event.msg else {
        panic!("expected request_permissions event");
    };
    assert_eq!(request.call_id, call_id);

    session
        .notify_request_permissions_response(&request.call_id, expected_response.clone())
        .await;

    let response = tokio::time::timeout(StdDuration::from_secs(1), handle)
        .await
        .expect("request_permissions future timed out")
        .expect("request_permissions join error");

    assert_eq!(response, Some(expected_response));
}

#[tokio::test]
async fn submit_with_id_captures_current_span_trace_context() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(1);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let chaos = Chaos {
        tx_sub,
        rx_event,
        agent_status,
        session: Arc::new(session),
        session_loop_termination: completed_session_loop_termination(),
    };

    init_test_tracing();

    let request_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000011-0000000000000022-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let request_span = info_span!("app_server.request");
    assert!(set_parent_from_w3c_trace_context(
        &request_span,
        &request_parent
    ));

    let expected_trace = async {
        let expected_trace =
            current_span_w3c_trace_context().expect("current span should have trace context");
        chaos
            .submit_with_id(Submission {
                id: "sub-1".into(),
                op: Op::Interrupt,
                trace: None,
            })
            .await
            .expect("submit should succeed");
        expected_trace
    }
    .instrument(request_span)
    .await;

    let submitted = rx_sub.recv().await.expect("submission");
    assert_eq!(submitted.trace, Some(expected_trace));
}

#[tokio::test]
async fn new_default_turn_captures_current_span_trace_id() {
    let (session, _turn_context) = make_session_and_context().await;

    init_test_tracing();

    let request_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000011-0000000000000022-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let request_span = info_span!("app_server.request");
    assert!(set_parent_from_w3c_trace_context(
        &request_span,
        &request_parent
    ));

    let turn_context_item = async {
        let expected_trace_id = Span::current()
            .context()
            .span()
            .span_context()
            .trace_id()
            .to_string();
        let turn_context = session.new_default_turn().await;
        let turn_context_item = turn_context.to_turn_context_item();
        assert_eq!(turn_context_item.trace_id, Some(expected_trace_id));
        turn_context_item
    }
    .instrument(request_span)
    .await;

    assert_eq!(
        turn_context_item.trace_id.as_deref(),
        Some("00000000000000000000000000000011")
    );
}

#[test]
fn submission_dispatch_span_prefers_submission_trace_context() {
    init_test_tracing();

    let ambient_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000033-0000000000000044-01".into()),
        tracestate: None,
    };
    let ambient_span = info_span!("ambient");
    assert!(set_parent_from_w3c_trace_context(
        &ambient_span,
        &ambient_parent
    ));

    let submission_trace = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000055-0000000000000066-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let dispatch_span = ambient_span.in_scope(|| {
        submission_dispatch_span(&Submission {
            id: "sub-1".into(),
            op: Op::Interrupt,
            trace: Some(submission_trace),
        })
    });

    let trace_id = dispatch_span.context().span().span_context().trace_id();
    assert_eq!(
        trace_id,
        TraceId::from_hex("00000000000000000000000000000055").expect("trace id")
    );
}

#[test]
fn op_kind_distinguishes_turn_ops() {
    assert_eq!(
        Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,

            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        }
        .kind(),
        "override_turn_context"
    );
    assert_eq!(
        Op::UserInput {
            items: vec![],
            final_output_json_schema: None,
        }
        .kind(),
        "user_input"
    );
}
