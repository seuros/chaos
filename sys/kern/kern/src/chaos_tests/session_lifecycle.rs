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
    session_configuration.sandbox_policy =
        chaos_sysctl::Constrained::allow_any(SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            read_only_access: ReadOnlyAccess::Restricted {
                include_platform_defaults: true,
                readable_roots: vec![docs_dir.clone()],
            },
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        });
    session_configuration.file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::CurrentWorkingDirectory,
            },
            access: FileSystemAccessMode::Write,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: docs_dir },
            access: FileSystemAccessMode::Read,
        },
    ]);

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(project_root),
            ..Default::default()
        })
        .expect("cwd-only update should succeed");

    assert_eq!(
        updated.file_system_sandbox_policy,
        session_configuration.file_system_sandbox_policy
    );
}

#[tokio::test]
async fn new_default_turn_uses_config_aware_skills_for_role_overrides() {
    let (session, _turn_context) = make_session_and_context().await;
    let parent_config = session.get_config().await;
    let chaos_home = parent_config.chaos_home.clone();
    let skill_dir = chaos_home.join("skills").join("demo");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    let skill_path = skill_dir.join("SKILL.md");
    std::fs::write(
        &skill_path,
        "---\nname: demo-skill\ndescription: demo description\n---\n\n# Body\n",
    )
    .expect("write skill");

    let parent_outcome = session
        .services
        .skills_manager
        .skills_for_cwd(&parent_config.cwd, true)
        .await;
    let parent_skill = parent_outcome
        .skills
        .iter()
        .find(|skill| skill.name == "demo-skill")
        .expect("demo skill should be discovered");
    assert!(parent_outcome.is_skill_enabled(parent_skill));

    let role_path = chaos_home.join("skills-role.toml");
    std::fs::write(
        &role_path,
        format!(
            r#"minion_instructions = "Stay focused"

[[skills.config]]
path = "{}"
enabled = false
"#,
            skill_path.display()
        ),
    )
    .expect("write role config");

    let mut child_config = (*parent_config).clone();
    child_config.agent_roles.insert(
        "custom".to_string(),
        crate::config::AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
            topics: None,
            catchphrases: None,
        },
    );
    crate::minions::role::apply_role_to_config(&mut child_config, Some("custom"))
        .await
        .expect("custom role should apply");

    {
        let mut state = session.state.lock().await;
        state.session_configuration.original_config_do_not_use = Arc::new(child_config);
    }

    let child_turn = session
        .new_default_turn_with_sub_id("role-skill-turn".to_string())
        .await;
    let child_skill = child_turn
        .turn_skills
        .outcome
        .skills
        .iter()
        .find(|skill| skill.name == "demo-skill")
        .expect("demo skill should be discovered");
    assert!(!child_turn.turn_skills.outcome.is_skill_enabled(child_skill));
}

#[tokio::test]
async fn session_configuration_apply_rederives_legacy_file_system_policy_on_cwd_update() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let workspace = tempfile::tempdir().expect("create temp dir");
    let project_root = workspace.path().join("project");
    let original_cwd = project_root.join("subdir");
    let docs_dir = original_cwd.join("docs");
    std::fs::create_dir_all(&docs_dir).expect("create docs dir");
    let docs_dir = chaos_realpath::AbsolutePathBuf::from_absolute_path(&docs_dir).expect("docs");

    session_configuration.cwd = original_cwd;
    session_configuration.sandbox_policy =
        chaos_sysctl::Constrained::allow_any(SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            read_only_access: ReadOnlyAccess::Restricted {
                include_platform_defaults: true,
                readable_roots: vec![docs_dir],
            },
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        });
    session_configuration.file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(
            session_configuration.sandbox_policy.get(),
            &session_configuration.cwd,
        );

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(project_root.clone()),
            ..Default::default()
        })
        .expect("cwd-only update should succeed");

    assert_eq!(
        updated.file_system_sandbox_policy,
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(
            updated.sandbox_policy.get(),
            &project_root,
        )
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
                skill_approval: true,
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
async fn request_permissions_is_auto_denied_when_granular_policy_blocks_tool_requests() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(crate::protocol::ApprovalPolicy::Granular(
            crate::protocol::GranularApprovalConfig {
                sandbox_approval: true,
                rules: true,
                skill_approval: true,
                request_permissions: false,
                mcp_elicitations: true,
            },
        ))
        .expect("test setup should allow updating approval policy");

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "call-1".to_string();
    let response = session
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
        .await;

    assert_eq!(
        response,
        Some(chaos_ipc::request_permissions::RequestPermissionsResponse {
            permissions: RequestPermissionProfile::default(),
            scope: PermissionGrantScope::Turn,
        })
    );
    assert!(
        tokio::time::timeout(StdDuration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "request_permissions should not emit an event when granular.request_permissions is false"
    );
}

#[tokio::test]
async fn submit_with_id_captures_current_span_trace_context() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(1);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let codex = Chaos {
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
        codex
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
            windows_sandbox_level: None,
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
