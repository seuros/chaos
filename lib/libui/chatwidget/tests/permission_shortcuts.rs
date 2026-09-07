use super::*;
use chaos_kern::config::Constrained;
use crossterm::event::KeyEventKind;
use pretty_assertions::assert_eq;

fn ctrl_p() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)
}

// Mirror the app's permission-event routing, without persisting operator config.
fn apply_permission_events(
    chat: &mut ChatWidget,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::ChaosOp(op) => assert!(chat.submit_op(op)),
            AppEvent::UpdateApprovalPolicy(policy) => chat.set_approval_policy(policy),
            AppEvent::UpdateSandboxPolicy(policy) => chat.set_sandbox_policy(policy).unwrap(),
            AppEvent::UpdateFullAccessWarningAcknowledged(ack) => {
                chat.set_full_access_warning_acknowledged(ack);
            }
            AppEvent::OpenPermissionsPopup
            | AppEvent::OpenApprovalsPopup
            | AppEvent::OpenFullAccessConfirmation { .. } => {
                panic!("cycling must not open a picker via app events");
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn ctrl_p_cycles_permissions_without_a_picker_and_preserves_draft() {
    for (running, draft) in [
        (false, ""),
        (true, ""),
        (false, "keep this draft\nand this line"),
        (true, "keep this draft\nand this line"),
    ] {
        let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
        let presets = builtin_approval_presets();
        let default = presets.iter().find(|preset| preset.id == "auto").unwrap();
        let full_access = presets
            .iter()
            .find(|preset| preset.id == "full-access")
            .unwrap();
        chat.set_approval_policy(default.approval);
        chat.set_sandbox_policy(default.sandbox.clone()).unwrap();
        chat.set_full_access_warning_acknowledged(true);
        chat.bottom_pane
            .set_composer_text(draft.into(), vec![], vec![]);
        if running {
            chat.on_task_started();
        }

        for selected in [full_access, default] {
            chat.handle_key_event(ctrl_p());
            assert!(chat.no_modal_or_popup_active(), "must not open the picker");
            assert_eq!(chat.bottom_pane.composer_text(), draft);
            apply_permission_events(&mut chat, &mut rx);
            assert_eq!(
                op_rx.try_recv().expect("live permissions update"),
                Op::UpdatePermissions {
                    scope: PermissionUpdateScope::Session,
                    expected_revision: None,
                    approval_policy: Some(selected.approval),
                    sandbox_policy: Some(selected.sandbox.clone()),
                    grants: PermissionGrantUpdate::Unchanged,
                }
            );
            assert!(
                op_rx.try_recv().is_err(),
                "must not interrupt or submit the draft"
            );
            assert_eq!(
                chat.config.permissions.approval_policy.value(),
                selected.approval
            );
            assert_eq!(
                chat.config.permissions.sandbox_policy.get(),
                &selected.sandbox
            );
            assert_eq!(chat.bottom_pane.is_task_running(), running);
            assert_eq!(chat.agent_turn_running, running);
        }
    }
}

#[tokio::test]
async fn ctrl_p_keeps_full_access_confirmation_and_cancel_returns_to_draft() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.set_approval_policy(ApprovalPolicy::Interactive);
    chat.set_sandbox_policy(SandboxPolicy::new_workspace_write_policy())
        .unwrap();
    chat.set_full_access_warning_acknowledged(false);
    chat.bottom_pane
        .set_composer_text("keep me".into(), vec![], vec![]);

    chat.handle_key_event(ctrl_p());
    assert!(render_bottom_popup(&chat, 80).contains("Enable full access?"));
    assert!(
        rx.try_recv().is_err(),
        "no permission changes before confirmation"
    );
    assert!(op_rx.try_recv().is_err());
    // Exercise the explicit Cancel action, not just generic Esc dismissal.
    chat.handle_key_event(KeyEvent::from(KeyCode::Char('3')));
    assert!(chat.no_modal_or_popup_active());
    assert_eq!(chat.bottom_pane.composer_text(), "keep me");
    assert!(
        rx.try_recv().is_err(),
        "cancel must not open the permissions picker"
    );
    assert_eq!(
        chat.config.permissions.approval_policy.value(),
        ApprovalPolicy::Interactive
    );

    chat.handle_key_event(ctrl_p());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    apply_permission_events(&mut chat, &mut rx);
    assert_matches!(
        op_rx.try_recv().unwrap(),
        Op::UpdatePermissions {
            approval_policy: Some(ApprovalPolicy::Headless),
            sandbox_policy: Some(SandboxPolicy::RootAccess),
            ..
        }
    );
    assert!(chat.config.notices.hide_full_access_warning.unwrap());
    assert_eq!(chat.bottom_pane.composer_text(), "keep me");
}

#[tokio::test]
async fn full_access_confirmation_persists_only_when_requested() {
    for (key, remember) in [(KeyCode::Enter, false), (KeyCode::Char('2'), true)] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
        let preset = builtin_approval_presets()
            .into_iter()
            .find(|preset| preset.id == "full-access")
            .unwrap();
        chat.open_full_access_confirmation(preset, true);
        chat.handle_key_event(KeyEvent::from(key));
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AppEvent::ChaosOp(Op::UpdatePermissions { .. })))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    AppEvent::UpdateFullAccessWarningAcknowledged(true)
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AppEvent::PersistFullAccessWarningAcknowledged))
                .count(),
            usize::from(remember)
        );
    }
}

#[tokio::test]
async fn ctrl_p_respects_approval_and_sandbox_constraints() {
    for constrain_approval in [false, true] {
        let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
        chat.set_approval_policy(ApprovalPolicy::Interactive);
        chat.set_sandbox_policy(SandboxPolicy::new_workspace_write_policy())
            .unwrap();
        chat.set_full_access_warning_acknowledged(true);
        if constrain_approval {
            chat.config.permissions.approval_policy =
                Constrained::allow_only(ApprovalPolicy::Interactive);
        } else {
            chat.config.permissions.sandbox_policy =
                Constrained::allow_only(SandboxPolicy::new_workspace_write_policy());
        }

        chat.handle_key_event(ctrl_p());
        assert!(chat.no_modal_or_popup_active());
        assert!(rx.try_recv().is_err(), "no alternative allowed preset");
        assert!(op_rx.try_recv().is_err());
        assert_eq!(
            chat.config.permissions.approval_policy.value(),
            ApprovalPolicy::Interactive
        );
        assert_eq!(
            chat.config.permissions.sandbox_policy.get(),
            &SandboxPolicy::new_workspace_write_policy()
        );
    }
}

#[tokio::test]
async fn ctrl_p_starts_at_default_for_an_unmatched_configuration() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.set_approval_policy(ApprovalPolicy::Headless);
    chat.set_sandbox_policy(SandboxPolicy::new_workspace_write_policy())
        .unwrap();

    chat.handle_key_event(ctrl_p());
    apply_permission_events(&mut chat, &mut rx);
    assert_matches!(
        op_rx.try_recv().unwrap(),
        Op::UpdatePermissions {
            approval_policy: Some(ApprovalPolicy::Interactive),
            sandbox_policy: Some(SandboxPolicy::WorkspaceWrite { .. }),
            ..
        }
    );
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn ctrl_p_preserves_popup_navigation() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.open_permissions_popup();
    let before = render_bottom_popup(&chat, 80);
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(ctrl_p());
    assert_eq!(render_bottom_popup(&chat, 80), before);
    chat.handle_key_event(KeyEvent::from(KeyCode::Esc));
    assert!(
        chat.no_modal_or_popup_active(),
        "must not stack a second popup"
    );
    assert!(rx.try_recv().is_err());
    assert!(op_rx.try_recv().is_err());

    chat.bottom_pane
        .set_composer_text("/".into(), vec![], vec![]);
    assert!(
        !chat.no_modal_or_popup_active(),
        "slash completion should be open"
    );
    let before = render_bottom_popup(&chat, 80);
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(ctrl_p());
    assert_eq!(render_bottom_popup(&chat, 80), before);
    assert_eq!(chat.bottom_pane.composer_text(), "/");
    assert!(rx.try_recv().is_err());
    assert!(op_rx.try_recv().is_err());
}

#[tokio::test]
async fn ctrl_p_ignores_repeats_and_releases_but_accepts_legacy_control_byte() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.set_approval_policy(ApprovalPolicy::Interactive);
    chat.set_sandbox_policy(SandboxPolicy::new_workspace_write_policy())
        .unwrap();
    chat.set_full_access_warning_acknowledged(true);
    for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
        chat.handle_key_event(KeyEvent { kind, ..ctrl_p() });
        assert!(chat.no_modal_or_popup_active());
        assert!(rx.try_recv().is_err());
        assert!(op_rx.try_recv().is_err());
    }
    chat.handle_key_event(KeyEvent::from(KeyCode::Char('\u{0010}')));
    apply_permission_events(&mut chat, &mut rx);
    assert_matches!(
        op_rx.try_recv().unwrap(),
        Op::UpdatePermissions {
            sandbox_policy: Some(SandboxPolicy::RootAccess),
            ..
        }
    );
}
