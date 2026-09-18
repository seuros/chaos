use super::*;
use chaos_ipc::config_types::ClampBackend;
use pretty_assertions::assert_eq;

struct RestoreTheme(bool);

impl Drop for RestoreTheme {
    fn drop(&mut self) {
        crate::theme::set_clamped(self.0);
    }
}

pub(super) async fn run() {
    let _restore = RestoreTheme(crate::theme::is_clamped());
    crate::theme::set_clamped(false);
    switching_backends_preserves_the_original_api_selection().await;
    configured_backend_is_used_by_toggle_and_startup().await;
    startup_event_preserves_the_selected_cli_model().await;
    invalid_and_busy_commands_do_not_change_transport().await;
    antigravity_model_picker_does_not_use_the_claude_catalog().await;
}

async fn switching_backends_preserves_the_original_api_selection() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::Low));
    chat.set_plan_mode_reasoning_effort(Some(ReasoningEffortConfig::Medium));
    while rx.try_recv().is_ok() {}

    for (arg, backend) in [
        ("agy", ClampBackend::Antigravity),
        ("antigravity", ClampBackend::Antigravity),
        ("claude", ClampBackend::ClaudeCode),
        ("claude-code", ClampBackend::ClaudeCode),
    ] {
        chat.dispatch_command_with_args(SlashCommand::Clamp, arg.into(), vec![]);
        assert!(crate::theme::is_clamped());
        assert_eq!(chat.config.clamp_backend, backend);
        assert_eq!(chat.model_display_name(), backend.display_name());
        assert_eq!(chat.pre_clamp_selection.as_ref().unwrap().model, "gpt-5");
        let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::ChaosOp(Op::SetClamped { enabled: true, backend: Some(selected) })
                if *selected == backend
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AppEvent::PersistModelSelection { .. }))
        );
        chat.add_status_output();
        let status = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => Some(
                    cell.display_lines(120)
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(status.contains(backend.display_name()), "{status}");
        assert!(status.contains(chat.current_model()), "{status}");
        if backend == ClampBackend::Antigravity {
            assert!(!status.contains("Claude Code"), "{status}");
        }
    }

    chat.dispatch_command_with_args(SlashCommand::Clamp, "off".into(), vec![]);
    assert!(!crate::theme::is_clamped());
    assert!(!chat.config.clamp);
    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::ChaosOp(Op::SetClamped { enabled: false, .. })
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::UpdateModel(model) if model == "gpt-5"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::UpdateReasoningEffort(Some(ReasoningEffortConfig::Low))
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::UpdatePlanModeReasoningEffort(Some(ReasoningEffortConfig::Medium))
    )));
    assert!(chat.pre_clamp_selection.is_none());
}

async fn configured_backend_is_used_by_toggle_and_startup() {
    for backend in [ClampBackend::ClaudeCode, ClampBackend::Antigravity] {
        let (mut chat, mut rx, _ops) = make_chatwidget_manual(Some("gpt-5")).await;
        chat.config.clamp_backend = backend;
        for startup in [false, true] {
            while rx.try_recv().is_ok() {}
            if startup {
                chat.activate_clamp();
            } else {
                chat.dispatch_command(SlashCommand::Clamp);
            }
            assert!(crate::theme::is_clamped());
            assert!(
                std::iter::from_fn(|| rx.try_recv().ok()).any(|event| matches!(
                    event,
                    AppEvent::ChaosOp(Op::SetClamped { enabled: true, backend: Some(selected) })
                        if selected == backend
                ))
            );
            chat.dispatch_command(SlashCommand::Clamp);
            assert!(!crate::theme::is_clamped());
        }
    }
}

async fn startup_event_preserves_the_selected_cli_model() {
    let (mut chat, mut rx, mut ops) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.config.clamp_backend = ClampBackend::Antigravity;
    chat.activate_clamp();
    let model = chat.current_model().to_string();
    let sandbox = SandboxPolicy::new_read_only_policy();
    chat.on_session_configured(chaos_ipc::protocol::SessionConfiguredEvent {
        session_id: ProcessId::new(),
        forked_from_id: None,
        process_name: None,
        model: "gpt-5".into(),
        model_provider_id: chat.config.model_provider_id.clone(),
        service_tier: None,
        approval_policy: ApprovalPolicy::Interactive,
        approvals_reviewer: chaos_ipc::config_types::ApprovalsReviewer::User,
        vfs_policy: chaos_ipc::protocol::VfsPolicy::from(&sandbox),
        socket_policy: chaos_ipc::protocol::SocketPolicy::from(&sandbox),
        cwd: chat.config.cwd.clone(),
        reasoning_effort: Some(ReasoningEffortConfig::Low),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        network_proxy: None,
    });
    assert_eq!(chat.current_model(), model);
    while ops.try_recv().is_ok() {}
    chat.submit_user_message(UserMessage::from("say ok"));
    assert!(
        std::iter::from_fn(|| ops.try_recv().ok()).any(|op| matches!(
            op,
            Op::UserTurn { model: selected, .. } if selected == model
        ))
    );
    chat.bottom_pane.set_task_running(false);
    chat.dispatch_command_with_args(SlashCommand::Clamp, "off".into(), vec![]);
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::UpdateModel(model) if model == "gpt-5"
        ))
    );
}

async fn invalid_and_busy_commands_do_not_change_transport() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(Some("gpt-5")).await;
    while rx.try_recv().is_ok() {}
    chat.dispatch_command_with_args(SlashCommand::Clamp, "unknown".into(), vec![]);
    chat.bottom_pane.set_task_running(true);
    chat.dispatch_command(SlashCommand::Clamp);
    chat.dispatch_command_with_args(SlashCommand::Clamp, "agy".into(), vec![]);
    assert!(!crate::theme::is_clamped());
    assert_eq!(chat.current_model(), "gpt-5");
    assert!(chat.pre_clamp_selection.is_none());
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::ChaosOp(Op::SetClamped { .. })))
    );
}

async fn antigravity_model_picker_does_not_use_the_claude_catalog() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.process_id = Some(ProcessId::new());
    chat.dispatch_command_with_args(SlashCommand::Clamp, "agy".into(), vec![]);
    while rx.try_recv().is_ok() {}
    chat.open_model_popup();
    if chat.config.antigravity.resolved().model.is_none() {
        let rendered = render_to_trimmed_string(&chat, Rect::new(0, 0, 100, 25));
        assert!(rendered.contains("Antigravity model"), "{rendered}");
        assert!(!rendered.contains("Claude Code"), "{rendered}");
        for c in "gemini-3.1-pro-high".chars() {
            chat.handle_key_event(KeyEvent::from(KeyCode::Char(c)));
        }
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert!(
            std::iter::from_fn(|| rx.try_recv().ok()).any(|event| matches!(
                event,
                AppEvent::UpdateModel(model) if model == "gemini-3.1-pro-high"
            ))
        );
    }
    chat.dispatch_command_with_args(SlashCommand::Clamp, "off".into(), vec![]);
}
