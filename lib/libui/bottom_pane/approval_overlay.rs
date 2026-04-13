mod format;
mod request;
mod state;

pub use format::format_requested_permissions_rule;
pub use request::ApprovalRequest;
pub use state::ApprovalOverlay;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use crate::bottom_pane::BottomPaneView;
    use crate::render::renderable::Renderable;
    use chaos_ipc::ProcessId;
    use chaos_ipc::mcp::RequestId;
    use chaos_ipc::models::FileSystemPermissions;
    use chaos_ipc::models::MacOsAutomationPermission;
    use chaos_ipc::models::MacOsPreferencesPermission;
    use chaos_ipc::models::MacOsSeatbeltProfileExtensions;
    use chaos_ipc::models::NetworkPermissions;
    use chaos_ipc::models::PermissionProfile;
    use chaos_ipc::protocol::ElicitationAction;
    use chaos_ipc::protocol::ExecPolicyAmendment;
    use chaos_ipc::protocol::NetworkApprovalContext;
    use chaos_ipc::protocol::NetworkApprovalProtocol;
    use chaos_ipc::protocol::NetworkPolicyAmendment;
    use chaos_ipc::protocol::NetworkPolicyRuleAction;
    use chaos_ipc::protocol::Op;
    use chaos_ipc::protocol::ReviewDecision;
    use chaos_ipc::request_permissions::PermissionGrantScope;
    use chaos_ipc::request_permissions::RequestPermissionProfile;
    use chaos_kern::features::Features;
    use chaos_realpath::AbsolutePathBuf;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use tokio::sync::mpsc::unbounded_channel;

    use super::super::CancellationEvent;
    use request::exec_options;
    use request::permissions_options;

    fn absolute_path(path: &str) -> AbsolutePathBuf {
        AbsolutePathBuf::from_absolute_path(path).expect("absolute path")
    }

    fn render_overlay_lines(view: &ApprovalOverlay, width: u16) -> String {
        let height = view.desired_height(width);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        view.render(Rect::new(0, 0, width, height), &mut buf);
        (0..buf.area.height)
            .map(|row| {
                (0..buf.area.width)
                    .map(|col| buf[(col, row)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn normalize_snapshot_paths(rendered: String) -> String {
        [
            (absolute_path("/tmp/readme.txt"), "/tmp/readme.txt"),
            (absolute_path("/tmp/out.txt"), "/tmp/out.txt"),
        ]
        .into_iter()
        .fold(rendered, |rendered, (path, normalized)| {
            rendered.replace(&path.display().to_string(), normalized)
        })
    }

    fn make_exec_request() -> ApprovalRequest {
        ApprovalRequest::Exec {
            process_id: ProcessId::new(),
            process_label: None,
            id: "test".to_string(),
            command: vec!["echo".to_string(), "hi".to_string()],
            reason: Some("reason".to_string()),
            available_decisions: vec![ReviewDecision::Approved, ReviewDecision::Abort],
            network_approval_context: None,
            additional_permissions: None,
        }
    }

    fn make_permissions_request() -> ApprovalRequest {
        ApprovalRequest::Permissions {
            process_id: ProcessId::new(),
            process_label: None,
            call_id: "test".to_string(),
            reason: Some("need workspace access".to_string()),
            permissions: RequestPermissionProfile {
                network: Some(NetworkPermissions {
                    enabled: Some(true),
                }),
                file_system: Some(FileSystemPermissions {
                    read: Some(vec![absolute_path("/tmp/readme.txt")]),
                    write: Some(vec![absolute_path("/tmp/out.txt")]),
                }),
            },
        }
    }

    fn make_url_elicitation_request() -> ApprovalRequest {
        ApprovalRequest::McpElicitation {
            process_id: ProcessId::new(),
            process_label: None,
            server_name: "calendar".to_string(),
            request_id: RequestId::Integer(7),
            message: "Open the provider login page to continue.".to_string(),
            url: Some("https://calendar.example.com/oauth/start".to_string()),
        }
    }

    #[test]
    fn ctrl_c_aborts_and_clears_queue() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let mut view = ApprovalOverlay::new(make_exec_request(), tx, Features::with_defaults());
        view.enqueue_request(make_exec_request());
        assert_eq!(CancellationEvent::Handled, view.on_ctrl_c());
        assert!(view.queue.is_empty());
        assert!(view.is_complete());
    }

    #[test]
    fn shortcut_triggers_selection() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let mut view = ApprovalOverlay::new(make_exec_request(), tx, Features::with_defaults());
        assert!(!view.is_complete());
        view.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        // We expect at least one process-scoped approval op message in the queue.
        let mut saw_op = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, AppEvent::SubmitProcessOp { .. }) {
                saw_op = true;
                break;
            }
        }
        assert!(saw_op, "expected approval decision to emit an op");
    }

    #[test]
    fn o_opens_source_process_for_cross_process_approval() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let process_id = ProcessId::new();
        let mut view = ApprovalOverlay::new(
            ApprovalRequest::Exec {
                process_id,
                process_label: Some("Robie [scout]".to_string()),
                id: "test".to_string(),
                command: vec!["echo".to_string(), "hi".to_string()],
                reason: None,
                available_decisions: vec![ReviewDecision::Approved, ReviewDecision::Abort],
                network_approval_context: None,
                additional_permissions: None,
            },
            tx,
            Features::with_defaults(),
        );

        view.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));

        let event = rx.try_recv().expect("expected select-agent-process event");
        assert_eq!(
            matches!(event, AppEvent::SelectAgentProcess(id) if id == process_id),
            true
        );
    }

    #[test]
    fn cross_process_footer_hint_mentions_o_shortcut() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let view = ApprovalOverlay::new(
            ApprovalRequest::Exec {
                process_id: ProcessId::new(),
                process_label: Some("Robie [scout]".to_string()),
                id: "test".to_string(),
                command: vec!["echo".to_string(), "hi".to_string()],
                reason: None,
                available_decisions: vec![ReviewDecision::Approved, ReviewDecision::Abort],
                network_approval_context: None,
                additional_permissions: None,
            },
            tx,
            Features::with_defaults(),
        );

        assert_snapshot!(
            "approval_overlay_cross_process_prompt",
            render_overlay_lines(&view, 80)
        );
    }

    #[test]
    fn mcp_url_elicitation_snapshot() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let view = ApprovalOverlay::new(
            make_url_elicitation_request(),
            tx,
            Features::with_defaults(),
        );

        assert_snapshot!(
            "approval_overlay_mcp_url_elicitation",
            render_overlay_lines(&view, 80)
        );
    }

    #[test]
    fn accepting_url_elicitation_opens_browser_and_resolves_on_success() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let request = make_url_elicitation_request();
        let process_id = request.process_id();
        let mut view = ApprovalOverlay::new(request, tx, Features::with_defaults());

        view.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        let event = rx
            .try_recv()
            .expect("expected browser-open elicitation event");
        match event {
            AppEvent::OpenUrlElicitationInBrowser {
                process_id: event_process_id,
                server_name,
                request_id,
                url,
                on_open,
                on_error,
            } => {
                assert_eq!(event_process_id, process_id);
                assert_eq!(server_name, "calendar");
                assert_eq!(request_id, RequestId::Integer(7));
                assert_eq!(url, "https://calendar.example.com/oauth/start");
                assert_eq!(on_open, ElicitationAction::Accept);
                assert_eq!(on_error, ElicitationAction::Cancel);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn exec_prefix_option_emits_execpolicy_amendment() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let mut view = ApprovalOverlay::new(
            ApprovalRequest::Exec {
                process_id: ProcessId::new(),
                process_label: None,
                id: "test".to_string(),
                command: vec!["echo".to_string()],
                reason: None,
                available_decisions: vec![
                    ReviewDecision::Approved,
                    ReviewDecision::ApprovedExecpolicyAmendment {
                        proposed_execpolicy_amendment: ExecPolicyAmendment::new(vec![
                            "echo".to_string(),
                        ]),
                    },
                    ReviewDecision::Abort,
                ],
                network_approval_context: None,
                additional_permissions: None,
            },
            tx,
            Features::with_defaults(),
        );
        view.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        let mut saw_op = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::SubmitProcessOp {
                op: Op::ExecApproval { decision, .. },
                ..
            } = ev
            {
                assert_eq!(
                    decision,
                    ReviewDecision::ApprovedExecpolicyAmendment {
                        proposed_execpolicy_amendment: ExecPolicyAmendment::new(vec![
                            "echo".to_string()
                        ])
                    }
                );
                saw_op = true;
                break;
            }
        }
        assert!(
            saw_op,
            "expected approval decision to emit an op with command prefix"
        );
    }

    #[test]
    fn network_deny_forever_shortcut_is_not_bound() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let mut view = ApprovalOverlay::new(
            ApprovalRequest::Exec {
                process_id: ProcessId::new(),
                process_label: None,
                id: "test".to_string(),
                command: vec!["curl".to_string(), "https://example.com".to_string()],
                reason: None,
                available_decisions: vec![
                    ReviewDecision::Approved,
                    ReviewDecision::ApprovedForSession,
                    ReviewDecision::NetworkPolicyAmendment {
                        network_policy_amendment: NetworkPolicyAmendment {
                            host: "example.com".to_string(),
                            action: NetworkPolicyRuleAction::Allow,
                        },
                    },
                    ReviewDecision::Abort,
                ],
                network_approval_context: Some(NetworkApprovalContext {
                    host: "example.com".to_string(),
                    protocol: NetworkApprovalProtocol::Https,
                }),
                additional_permissions: None,
            },
            tx,
            Features::with_defaults(),
        );
        view.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

        assert!(
            rx.try_recv().is_err(),
            "unexpected approval event emitted for hidden network deny shortcut"
        );
    }

    #[test]
    fn header_includes_command_snippet() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let command = vec!["echo".into(), "hello".into(), "world".into()];
        let exec_request = ApprovalRequest::Exec {
            process_id: ProcessId::new(),
            process_label: None,
            id: "test".into(),
            command,
            reason: None,
            available_decisions: vec![ReviewDecision::Approved, ReviewDecision::Abort],
            network_approval_context: None,
            additional_permissions: None,
        };

        let view = ApprovalOverlay::new(exec_request, tx, Features::with_defaults());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, view.desired_height(80)));
        view.render(Rect::new(0, 0, 80, view.desired_height(80)), &mut buf);

        let rendered: Vec<String> = (0..buf.area.height)
            .map(|row| {
                (0..buf.area.width)
                    .map(|col| buf[(col, row)].symbol().to_string())
                    .collect()
            })
            .collect();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("echo hello world")),
            "expected header to include command snippet, got {rendered:?}"
        );
    }

    #[test]
    fn network_exec_options_use_expected_labels_and_hide_execpolicy_amendment() {
        let network_context = NetworkApprovalContext {
            host: "example.com".to_string(),
            protocol: NetworkApprovalProtocol::Https,
        };
        let options = exec_options(
            &[
                ReviewDecision::Approved,
                ReviewDecision::ApprovedForSession,
                ReviewDecision::NetworkPolicyAmendment {
                    network_policy_amendment: NetworkPolicyAmendment {
                        host: "example.com".to_string(),
                        action: NetworkPolicyRuleAction::Allow,
                    },
                },
                ReviewDecision::Abort,
            ],
            Some(&network_context),
            None,
        );

        let labels: Vec<String> = options.into_iter().map(|option| option.label).collect();
        assert_eq!(
            labels,
            vec![
                "Yes, just this once".to_string(),
                "Yes, and allow this host for this conversation".to_string(),
                "Yes, and allow this host in the future".to_string(),
                "No, and tell Chaos what to do differently".to_string(),
            ]
        );
    }

    #[test]
    fn generic_exec_options_can_offer_allow_for_session() {
        let options = exec_options(
            &[
                ReviewDecision::Approved,
                ReviewDecision::ApprovedForSession,
                ReviewDecision::Abort,
            ],
            None,
            None,
        );

        let labels: Vec<String> = options.into_iter().map(|option| option.label).collect();
        assert_eq!(
            labels,
            vec![
                "Yes, proceed".to_string(),
                "Yes, and don't ask again for this command in this session".to_string(),
                "No, and tell Chaos what to do differently".to_string(),
            ]
        );
    }

    #[test]
    fn additional_permissions_exec_options_hide_execpolicy_amendment() {
        let additional_permissions = PermissionProfile {
            file_system: Some(FileSystemPermissions {
                read: Some(vec![absolute_path("/tmp/readme.txt")]),
                write: Some(vec![absolute_path("/tmp/out.txt")]),
            }),
            ..Default::default()
        };
        let options = exec_options(
            &[ReviewDecision::Approved, ReviewDecision::Abort],
            None,
            Some(&additional_permissions),
        );

        let labels: Vec<String> = options.into_iter().map(|option| option.label).collect();
        assert_eq!(
            labels,
            vec![
                "Yes, proceed".to_string(),
                "No, and tell Chaos what to do differently".to_string(),
            ]
        );
    }

    #[test]
    fn permissions_options_use_expected_labels() {
        let labels: Vec<String> = permissions_options()
            .into_iter()
            .map(|option| option.label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "Yes, grant these permissions".to_string(),
                "Yes, grant these permissions for this session".to_string(),
                "No, continue without permissions".to_string(),
            ]
        );
    }

    #[test]
    fn permissions_session_shortcut_submits_session_scope() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let mut view =
            ApprovalOverlay::new(make_permissions_request(), tx, Features::with_defaults());

        view.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));

        let mut saw_op = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::SubmitProcessOp {
                op: Op::RequestPermissionsResponse { response, .. },
                ..
            } = ev
            {
                assert_eq!(response.scope, PermissionGrantScope::Session);
                saw_op = true;
                break;
            }
        }
        assert!(
            saw_op,
            "expected permission approval decision to emit a session-scoped response"
        );
    }

    #[test]
    fn additional_permissions_prompt_shows_permission_rule_line() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let exec_request = ApprovalRequest::Exec {
            process_id: ProcessId::new(),
            process_label: None,
            id: "test".into(),
            command: vec!["cat".into(), "/tmp/readme.txt".into()],
            reason: None,
            available_decisions: vec![ReviewDecision::Approved, ReviewDecision::Abort],
            network_approval_context: None,
            additional_permissions: Some(PermissionProfile {
                network: Some(NetworkPermissions {
                    enabled: Some(true),
                }),
                file_system: Some(FileSystemPermissions {
                    read: Some(vec![absolute_path("/tmp/readme.txt")]),
                    write: Some(vec![absolute_path("/tmp/out.txt")]),
                }),
                ..Default::default()
            }),
        };

        let view = ApprovalOverlay::new(exec_request, tx, Features::with_defaults());
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, view.desired_height(120)));
        view.render(Rect::new(0, 0, 120, view.desired_height(120)), &mut buf);

        let rendered: Vec<String> = (0..buf.area.height)
            .map(|row| {
                (0..buf.area.width)
                    .map(|col| buf[(col, row)].symbol().to_string())
                    .collect()
            })
            .collect();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Permission rule:")),
            "expected permission-rule line, got {rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.contains("network;")),
            "expected network permission text, got {rendered:?}"
        );
    }

    #[test]
    fn additional_permissions_prompt_snapshot() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let exec_request = ApprovalRequest::Exec {
            process_id: ProcessId::new(),
            process_label: None,
            id: "test".into(),
            command: vec!["cat".into(), "/tmp/readme.txt".into()],
            reason: Some("need filesystem access".into()),
            available_decisions: vec![ReviewDecision::Approved, ReviewDecision::Abort],
            network_approval_context: None,
            additional_permissions: Some(PermissionProfile {
                network: Some(NetworkPermissions {
                    enabled: Some(true),
                }),
                file_system: Some(FileSystemPermissions {
                    read: Some(vec![absolute_path("/tmp/readme.txt")]),
                    write: Some(vec![absolute_path("/tmp/out.txt")]),
                }),
                ..Default::default()
            }),
        };

        let view = ApprovalOverlay::new(exec_request, tx, Features::with_defaults());
        assert_snapshot!(
            "approval_overlay_additional_permissions_prompt",
            normalize_snapshot_paths(render_overlay_lines(&view, 120))
        );
    }

    #[test]
    fn permissions_prompt_snapshot() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let view = ApprovalOverlay::new(make_permissions_request(), tx, Features::with_defaults());
        assert_snapshot!(
            "approval_overlay_permissions_prompt",
            normalize_snapshot_paths(render_overlay_lines(&view, 120))
        );
    }

    #[test]
    fn additional_permissions_macos_prompt_snapshot() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let exec_request = ApprovalRequest::Exec {
            process_id: ProcessId::new(),
            process_label: None,
            id: "test".into(),
            command: vec!["osascript".into(), "-e".into(), "tell application".into()],
            reason: Some("need macOS automation".into()),
            available_decisions: vec![ReviewDecision::Approved, ReviewDecision::Abort],
            network_approval_context: None,
            additional_permissions: Some(PermissionProfile {
                macos: Some(MacOsSeatbeltProfileExtensions {
                    macos_preferences: MacOsPreferencesPermission::ReadWrite,
                    macos_automation: MacOsAutomationPermission::BundleIds(vec![
                        "com.apple.Calendar".to_string(),
                        "com.apple.Notes".to_string(),
                    ]),
                    macos_launch_services: false,
                    macos_accessibility: true,
                    macos_calendar: true,
                    macos_reminders: true,
                    macos_contacts: chaos_ipc::models::MacOsContactsPermission::None,
                }),
                ..Default::default()
            }),
        };

        let view = ApprovalOverlay::new(exec_request, tx, Features::with_defaults());
        assert_snapshot!(
            "approval_overlay_additional_permissions_macos_prompt",
            render_overlay_lines(&view, 120)
        );
    }

    #[test]
    fn network_exec_prompt_title_includes_host() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let exec_request = ApprovalRequest::Exec {
            process_id: ProcessId::new(),
            process_label: None,
            id: "test".into(),
            command: vec!["curl".into(), "https://example.com".into()],
            reason: Some("network request blocked".into()),
            available_decisions: vec![
                ReviewDecision::Approved,
                ReviewDecision::ApprovedForSession,
                ReviewDecision::NetworkPolicyAmendment {
                    network_policy_amendment: NetworkPolicyAmendment {
                        host: "example.com".to_string(),
                        action: NetworkPolicyRuleAction::Allow,
                    },
                },
                ReviewDecision::Abort,
            ],
            network_approval_context: Some(NetworkApprovalContext {
                host: "example.com".to_string(),
                protocol: NetworkApprovalProtocol::Https,
            }),
            additional_permissions: None,
        };

        let view = ApprovalOverlay::new(exec_request, tx, Features::with_defaults());
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, view.desired_height(100)));
        view.render(Rect::new(0, 0, 100, view.desired_height(100)), &mut buf);
        assert_snapshot!("network_exec_prompt", format!("{buf:?}"));

        let rendered: Vec<String> = (0..buf.area.height)
            .map(|row| {
                (0..buf.area.width)
                    .map(|col| buf[(col, row)].symbol().to_string())
                    .collect()
            })
            .collect();

        assert!(
            rendered.iter().any(|line| {
                line.contains("Do you want to approve network access to \"example.com\"?")
            }),
            "expected network title to include host, got {rendered:?}"
        );
        assert!(
            !rendered.iter().any(|line| line.contains("$ curl")),
            "network prompt should not show command line, got {rendered:?}"
        );
        assert!(
            !rendered.iter().any(|line| line.contains("don't ask again")),
            "network prompt should not show execpolicy option, got {rendered:?}"
        );
    }

    #[test]
    fn exec_history_cell_wraps_with_two_space_indent() {
        use crate::history_cell;
        let command = vec![
            "/bin/zsh".into(),
            "-lc".into(),
            "git add tui/src/render/mod.rs tui/src/render/renderable.rs".into(),
        ];
        let cell = history_cell::new_approval_decision_cell(
            command,
            ReviewDecision::Approved,
            history_cell::ApprovalDecisionActor::User,
        );
        let lines = cell.display_lines(28);
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        let expected = vec![
            "✔ You approved chaos to run".to_string(),
            "  git add tui/src/render/".to_string(),
            "  mod.rs tui/src/render/".to_string(),
            "  renderable.rs this time".to_string(),
        ];
        assert_eq!(rendered, expected);
    }

    #[test]
    fn enter_sets_last_selected_index_without_dismissing() {
        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut view = ApprovalOverlay::new(make_exec_request(), tx, Features::with_defaults());
        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(
            view.is_complete(),
            "exec approval should complete without queued requests"
        );

        let mut decision = None;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::SubmitProcessOp {
                op: Op::ExecApproval { decision: d, .. },
                ..
            } = ev
            {
                decision = Some(d);
                break;
            }
        }
        assert_eq!(decision, Some(ReviewDecision::Approved));
    }
}
