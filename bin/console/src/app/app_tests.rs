use super::*;
use crate::app_backtrack::BacktrackSelection;
use crate::app_backtrack::BacktrackState;
use crate::app_backtrack::user_count;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use crate::file_search::FileSearchManager;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::UserHistoryCell;
use crate::history_cell::new_session_info;
use crate::multi_agents::AgentPickerProcessEntry;
use assert_matches::assert_matches;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::config_types::Settings;
use chaos_ipc::protocol::AgentMessageContentDeltaEvent;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ProcessRolledBackEvent;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionConfiguredEvent;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::TurnAbortReason;
use chaos_ipc::protocol::TurnAbortedEvent;
use chaos_ipc::protocol::TurnCompleteEvent;
use chaos_ipc::protocol::TurnStartedEvent;
use chaos_ipc::protocol::UserMessageEvent;
use chaos_ipc::user_input::TextElement;
use chaos_ipc::user_input::UserInput;
use chaos_kern::ChaosAuth;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::types::ApprovalsReviewer;
use chaos_syslog::SessionTelemetry;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::prelude::Line;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;
use tokio::time;

#[test]
fn normalize_harness_overrides_resolves_relative_add_dirs() -> Result<()> {
    let temp_dir = tempdir()?;
    let base_cwd = temp_dir.path().join("base");
    std::fs::create_dir_all(&base_cwd)?;

    let overrides = ConfigOverrides {
        additional_writable_roots: vec![PathBuf::from("rel")],
        ..Default::default()
    };
    let normalized = normalize_harness_overrides_for_cwd(overrides, &base_cwd)?;

    assert_eq!(
        normalized.additional_writable_roots,
        vec![base_cwd.join("rel")]
    );
    Ok(())
}

#[test]
fn startup_waiting_gate_is_only_for_fresh_or_exit_session_selection() {
    assert_eq!(
        App::should_wait_for_initial_session(&SessionSelection::StartFresh),
        true
    );
    assert_eq!(
        App::should_wait_for_initial_session(&SessionSelection::Exit),
        true
    );
    assert_eq!(
        App::should_wait_for_initial_session(&SessionSelection::Resume(
            crate::resume_picker::SessionTarget {
                process_id: ProcessId::new(),
            }
        )),
        false
    );
    assert_eq!(
        App::should_wait_for_initial_session(&SessionSelection::Fork(
            crate::resume_picker::SessionTarget {
                process_id: ProcessId::new(),
            }
        )),
        false
    );
}

#[test]
fn startup_waiting_gate_holds_active_process_events_until_primary_process_configured() {
    let mut wait_for_initial_session =
        App::should_wait_for_initial_session(&SessionSelection::StartFresh);
    assert_eq!(wait_for_initial_session, true);
    assert_eq!(
        App::should_handle_active_process_events(wait_for_initial_session, true),
        false
    );

    assert_eq!(
        App::should_stop_waiting_for_initial_session(wait_for_initial_session, None),
        false
    );
    if App::should_stop_waiting_for_initial_session(
        wait_for_initial_session,
        Some(ProcessId::new()),
    ) {
        wait_for_initial_session = false;
    }
    assert_eq!(wait_for_initial_session, false);

    assert_eq!(
        App::should_handle_active_process_events(wait_for_initial_session, true),
        true
    );
}

#[test]
fn startup_waiting_gate_not_applied_for_resume_or_fork_session_selection() {
    let wait_for_resume = App::should_wait_for_initial_session(&SessionSelection::Resume(
        crate::resume_picker::SessionTarget {
            process_id: ProcessId::new(),
        },
    ));
    assert_eq!(
        App::should_handle_active_process_events(wait_for_resume, true),
        true
    );
    let wait_for_fork = App::should_wait_for_initial_session(&SessionSelection::Fork(
        crate::resume_picker::SessionTarget {
            process_id: ProcessId::new(),
        },
    ));
    assert_eq!(
        App::should_handle_active_process_events(wait_for_fork, true),
        true
    );
}

#[tokio::test]
async fn enqueue_primary_event_delivers_session_configured_before_buffered_approval() -> Result<()>
{
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    let approval_event = Event {
        id: "approval-event".to_string(),
        msg: EventMsg::ExecApprovalRequest(chaos_ipc::protocol::ExecApprovalRequestEvent {
            call_id: "call-1".to_string(),
            approval_id: None,
            turn_id: "turn-1".to_string(),
            command: vec!["echo".to_string(), "hello".to_string()],
            cwd: PathBuf::from("/tmp/project"),
            reason: Some("needs approval".to_string()),
            network_approval_context: None,
            proposed_execpolicy_amendment: None,
            proposed_network_policy_amendments: None,
            additional_permissions: None,
            available_decisions: None,
            parsed_cmd: Vec::new(),
        }),
    };
    let session_configured_event = Event {
        id: "session-configured".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    };

    app.enqueue_primary_event(approval_event.clone()).await?;
    app.enqueue_primary_event(session_configured_event.clone())
        .await?;

    let rx = app
        .active_process_rx
        .as_mut()
        .expect("primary thread receiver should be active");
    let first_event = time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .expect("timed out waiting for session configured event")
        .expect("channel closed unexpectedly");
    let second_event = time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .expect("timed out waiting for buffered approval event")
        .expect("channel closed unexpectedly");

    assert!(matches!(first_event.msg, EventMsg::SessionConfigured(_)));
    assert!(matches!(second_event.msg, EventMsg::ExecApprovalRequest(_)));

    app.handle_codex_event_now(first_event);
    app.handle_codex_event_now(second_event);
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

    while let Ok(app_event) = app_event_rx.try_recv() {
        if let AppEvent::SubmitProcessOp {
            process_id: op_process_id,
            ..
        } = app_event
        {
            assert_eq!(op_process_id, process_id);
            return Ok(());
        }
    }

    panic!("expected approval action to submit a process-scoped op");
}

#[tokio::test]
async fn routed_thread_event_does_not_recreate_channel_after_reset() -> Result<()> {
    let mut app = make_test_app().await;
    let process_id = ProcessId::new();
    app.process_event_channels.insert(
        process_id,
        ProcessEventChannel::new(PROCESS_EVENT_CHANNEL_CAPACITY),
    );

    app.reset_process_event_state();
    app.handle_routed_process_event(
        process_id,
        Event {
            id: "stale-event".to_string(),
            msg: EventMsg::ShutdownComplete,
        },
    )
    .await?;

    assert!(
        !app.process_event_channels.contains_key(&process_id),
        "stale routed events should not recreate cleared thread channels"
    );
    assert_eq!(app.active_process_id, None);
    assert_eq!(app.primary_process_id, None);
    Ok(())
}

#[tokio::test]
async fn reset_process_event_state_aborts_listener_tasks() {
    struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    let mut app = make_test_app().await;
    let process_id = ProcessId::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _notify_on_drop = NotifyOnDrop(Some(dropped_tx));
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    });
    app.process_event_listener_tasks.insert(process_id, handle);
    started_rx
        .await
        .expect("listener task should report it started");

    app.reset_process_event_state();

    assert_eq!(app.process_event_listener_tasks.is_empty(), true);
    time::timeout(Duration::from_millis(50), dropped_rx)
        .await
        .expect("timed out waiting for listener task abort")
        .expect("listener task drop notification should succeed");
}

#[tokio::test]
async fn enqueue_thread_event_does_not_block_when_channel_full() -> Result<()> {
    let mut app = make_test_app().await;
    let process_id = ProcessId::new();
    app.process_event_channels
        .insert(process_id, ProcessEventChannel::new(1));
    app.set_process_active(process_id, true).await;

    let event = Event {
        id: String::new(),
        msg: EventMsg::ShutdownComplete,
    };

    app.enqueue_process_event(process_id, event.clone()).await?;
    time::timeout(
        Duration::from_millis(50),
        app.enqueue_process_event(process_id, event),
    )
    .await
    .expect("enqueue_process_event blocked on a full channel")?;

    let mut rx = app
        .process_event_channels
        .get_mut(&process_id)
        .expect("missing thread channel")
        .receiver
        .take()
        .expect("missing receiver");

    time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .expect("timed out waiting for first event")
        .expect("channel closed unexpectedly");
    time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .expect("timed out waiting for second event")
        .expect("channel closed unexpectedly");

    Ok(())
}

#[tokio::test]
async fn replay_process_snapshot_restores_draft_and_queued_input() {
    let mut app = make_test_app().await;
    let process_id = ProcessId::new();
    app.process_event_channels.insert(
        process_id,
        ProcessEventChannel::new_with_session_configured(
            PROCESS_EVENT_CHANNEL_CAPACITY,
            Event {
                id: "session-configured".to_string(),
                msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                    session_id: process_id,
                    forked_from_id: None,
                    process_name: None,
                    model: "gpt-test".to_string(),
                    model_provider_id: "test-provider".to_string(),
                    service_tier: None,
                    approval_policy: ApprovalPolicy::Headless,
                    approvals_reviewer: ApprovalsReviewer::User,
                    file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                        &SandboxPolicy::new_read_only_policy(),
                    ),
                    network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                        &SandboxPolicy::new_read_only_policy(),
                    ),
                    cwd: PathBuf::from("/tmp/project"),
                    reasoning_effort: None,
                    history_log_id: 0,
                    history_entry_count: 0,
                    initial_messages: None,
                    network_proxy: None,
                }),
            },
        ),
    );
    app.activate_process_channel(process_id).await;

    app.chat_widget
        .apply_external_edit("draft prompt".to_string());
    app.chat_widget.submit_user_message_with_mode(
        "queued follow-up".to_string(),
        CollaborationModeMask {
            name: "Default".to_string(),
            mode: None,
            model: None,
            reasoning_effort: None,
            minion_instructions: None,
        },
    );
    let expected_input_state = app
        .chat_widget
        .capture_process_input_state()
        .expect("expected thread input state");

    app.store_active_process_receiver().await;

    let snapshot = {
        let channel = app
            .process_event_channels
            .get(&process_id)
            .expect("thread channel should exist");
        let store = channel.store.lock().await;
        assert_eq!(store.input_state, Some(expected_input_state));
        store.snapshot()
    };

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;

    app.replay_process_snapshot(snapshot, true);

    assert_eq!(app.chat_widget.composer_text_with_pending(), "draft prompt");
    assert!(app.chat_widget.queued_user_message_texts().is_empty());
    match next_user_turn_op(&mut new_op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "queued follow-up".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected queued follow-up submission, got {other:?}"),
    }
}

#[tokio::test]
async fn replayed_turn_complete_submits_restored_queued_follow_up() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    let session_configured = Event {
        id: "session-configured".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    };
    app.chat_widget
        .handle_codex_event(session_configured.clone());
    app.chat_widget.handle_codex_event(Event {
        id: "turn-started".to_string(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
    });
    app.chat_widget.handle_codex_event(Event {
        id: "agent-delta".to_string(),
        msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
            process_id: String::new(),
            turn_id: String::new(),
            item_id: String::new(),
            delta: "streaming".to_string(),
        }),
    });
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_process_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_codex_event(session_configured);
    while new_op_rx.try_recv().is_ok() {}
    app.replay_process_snapshot(
        ProcessEventSnapshot {
            session_configured: None,
            events: vec![Event {
                id: "turn-complete".to_string(),
                msg: EventMsg::TurnComplete(TurnCompleteEvent {
                    turn_id: "turn-1".to_string(),
                    last_agent_message: None,
                }),
            }],
            input_state: Some(input_state),
        },
        true,
    );

    match next_user_turn_op(&mut new_op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "queued follow-up".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected queued follow-up submission, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_only_thread_keeps_restored_queue_visible() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    let session_configured = Event {
        id: "session-configured".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    };
    app.chat_widget
        .handle_codex_event(session_configured.clone());
    app.chat_widget.handle_codex_event(Event {
        id: "turn-started".to_string(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
    });
    app.chat_widget.handle_codex_event(Event {
        id: "agent-delta".to_string(),
        msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
            process_id: String::new(),
            turn_id: String::new(),
            item_id: String::new(),
            delta: "streaming".to_string(),
        }),
    });
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_process_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_codex_event(session_configured);
    while new_op_rx.try_recv().is_ok() {}

    app.replay_process_snapshot(
        ProcessEventSnapshot {
            session_configured: None,
            events: vec![Event {
                id: "turn-complete".to_string(),
                msg: EventMsg::TurnComplete(TurnCompleteEvent {
                    turn_id: "turn-1".to_string(),
                    last_agent_message: None,
                }),
            }],
            input_state: Some(input_state),
        },
        false,
    );

    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up".to_string()]
    );
    assert!(
        new_op_rx.try_recv().is_err(),
        "replay-only threads should not auto-submit restored queue"
    );
}

#[tokio::test]
async fn replay_process_snapshot_keeps_queue_when_running_state_only_comes_from_snapshot() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    let session_configured = Event {
        id: "session-configured".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    };
    app.chat_widget
        .handle_codex_event(session_configured.clone());
    app.chat_widget.handle_codex_event(Event {
        id: "turn-started".to_string(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
    });
    app.chat_widget.handle_codex_event(Event {
        id: "agent-delta".to_string(),
        msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
            process_id: String::new(),
            turn_id: String::new(),
            item_id: String::new(),
            delta: "streaming".to_string(),
        }),
    });
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_process_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_codex_event(session_configured);
    while new_op_rx.try_recv().is_ok() {}

    app.replay_process_snapshot(
        ProcessEventSnapshot {
            session_configured: None,
            events: vec![],
            input_state: Some(input_state),
        },
        true,
    );

    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up".to_string()]
    );
    assert!(
        new_op_rx.try_recv().is_err(),
        "restored queue should stay queued when replay did not prove the turn finished"
    );
}

#[tokio::test]
async fn replay_process_snapshot_does_not_submit_queue_before_replay_catches_up() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    let session_configured = Event {
        id: "session-configured".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    };
    app.chat_widget
        .handle_codex_event(session_configured.clone());
    app.chat_widget.handle_codex_event(Event {
        id: "turn-started".to_string(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
    });
    app.chat_widget.handle_codex_event(Event {
        id: "agent-delta".to_string(),
        msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
            process_id: String::new(),
            turn_id: String::new(),
            item_id: String::new(),
            delta: "streaming".to_string(),
        }),
    });
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_process_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_codex_event(session_configured);
    while new_op_rx.try_recv().is_ok() {}

    app.replay_process_snapshot(
        ProcessEventSnapshot {
            session_configured: None,
            events: vec![
                Event {
                    id: "older-turn-complete".to_string(),
                    msg: EventMsg::TurnComplete(TurnCompleteEvent {
                        turn_id: "turn-0".to_string(),
                        last_agent_message: None,
                    }),
                },
                Event {
                    id: "latest-turn-started".to_string(),
                    msg: EventMsg::TurnStarted(TurnStartedEvent {
                        turn_id: "turn-1".to_string(),
                        model_context_window: None,
                        collaboration_mode_kind: Default::default(),
                    }),
                },
            ],
            input_state: Some(input_state),
        },
        true,
    );

    assert!(
        new_op_rx.try_recv().is_err(),
        "queued follow-up should stay queued until the latest turn completes"
    );
    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up".to_string()]
    );

    app.chat_widget.handle_codex_event(Event {
        id: "latest-turn-complete".to_string(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
        }),
    });

    match next_user_turn_op(&mut new_op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "queued follow-up".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected queued follow-up submission, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_process_snapshot_restores_pending_pastes_for_submit() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    app.process_event_channels.insert(
        process_id,
        ProcessEventChannel::new_with_session_configured(
            PROCESS_EVENT_CHANNEL_CAPACITY,
            Event {
                id: "session-configured".to_string(),
                msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                    session_id: process_id,
                    forked_from_id: None,
                    process_name: None,
                    model: "gpt-test".to_string(),
                    model_provider_id: "test-provider".to_string(),
                    service_tier: None,
                    approval_policy: ApprovalPolicy::Headless,
                    approvals_reviewer: ApprovalsReviewer::User,
                    file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                        &SandboxPolicy::new_read_only_policy(),
                    ),
                    network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                        &SandboxPolicy::new_read_only_policy(),
                    ),
                    cwd: PathBuf::from("/tmp/project"),
                    reasoning_effort: None,
                    history_log_id: 0,
                    history_entry_count: 0,
                    initial_messages: None,
                    network_proxy: None,
                }),
            },
        ),
    );
    app.activate_process_channel(process_id).await;

    let large = "x".repeat(1005);
    app.chat_widget.handle_paste(large.clone());
    let expected_input_state = app
        .chat_widget
        .capture_process_input_state()
        .expect("expected thread input state");

    app.store_active_process_receiver().await;

    let snapshot = {
        let channel = app
            .process_event_channels
            .get(&process_id)
            .expect("thread channel should exist");
        let store = channel.store.lock().await;
        assert_eq!(store.input_state, Some(expected_input_state));
        store.snapshot()
    };

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.replay_process_snapshot(snapshot, true);

    assert_eq!(app.chat_widget.composer_text_with_pending(), large);

    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match next_user_turn_op(&mut new_op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: large,
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected restored paste submission, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_process_snapshot_restores_collaboration_mode_for_draft_submit() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    let session_configured = Event {
        id: "session-configured".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    };
    app.chat_widget
        .handle_codex_event(session_configured.clone());
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.chat_widget
        .set_collaboration_mask(CollaborationModeMask {
            name: "Plan".to_string(),
            mode: Some(ModeKind::Plan),
            model: Some("gpt-restored".to_string()),
            reasoning_effort: Some(Some(ReasoningEffortConfig::High)),
            minion_instructions: None,
        });
    app.chat_widget
        .apply_external_edit("draft prompt".to_string());
    let input_state = app
        .chat_widget
        .capture_process_input_state()
        .expect("expected draft input state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_codex_event(session_configured);
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::Low));
    app.chat_widget
        .set_collaboration_mask(CollaborationModeMask {
            name: "Default".to_string(),
            mode: Some(ModeKind::Default),
            model: Some("gpt-replacement".to_string()),
            reasoning_effort: Some(Some(ReasoningEffortConfig::Low)),
            minion_instructions: None,
        });
    while new_op_rx.try_recv().is_ok() {}

    app.replay_process_snapshot(
        ProcessEventSnapshot {
            session_configured: None,
            events: vec![],
            input_state: Some(input_state),
        },
        true,
    );
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match next_user_turn_op(&mut new_op_rx) {
        Op::UserTurn {
            items,
            model,
            effort,
            collaboration_mode,
            ..
        } => {
            assert_eq!(
                items,
                vec![UserInput::Text {
                    text: "draft prompt".to_string(),
                    text_elements: Vec::new(),
                }]
            );
            assert_eq!(model, "gpt-restored".to_string());
            assert_eq!(effort, Some(ReasoningEffortConfig::High));
            assert_eq!(
                collaboration_mode,
                Some(CollaborationMode {
                    mode: ModeKind::Plan,
                    settings: Settings {
                        model: "gpt-restored".to_string(),
                        reasoning_effort: Some(ReasoningEffortConfig::High),
                        minion_instructions: None,
                    },
                })
            );
        }
        other => panic!("expected restored draft submission, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_process_snapshot_restores_collaboration_mode_without_input() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    let session_configured = Event {
        id: "session-configured".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    };
    app.chat_widget
        .handle_codex_event(session_configured.clone());
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.chat_widget
        .set_collaboration_mask(CollaborationModeMask {
            name: "Plan".to_string(),
            mode: Some(ModeKind::Plan),
            model: Some("gpt-restored".to_string()),
            reasoning_effort: Some(Some(ReasoningEffortConfig::High)),
            minion_instructions: None,
        });
    let input_state = app
        .chat_widget
        .capture_process_input_state()
        .expect("expected collaboration-only input state");

    let (chat_widget, _app_event_tx, _rx, _new_op_rx) = make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_codex_event(session_configured);
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::Low));
    app.chat_widget
        .set_collaboration_mask(CollaborationModeMask {
            name: "Default".to_string(),
            mode: Some(ModeKind::Default),
            model: Some("gpt-replacement".to_string()),
            reasoning_effort: Some(Some(ReasoningEffortConfig::Low)),
            minion_instructions: None,
        });

    app.replay_process_snapshot(
        ProcessEventSnapshot {
            session_configured: None,
            events: vec![],
            input_state: Some(input_state),
        },
        true,
    );

    assert_eq!(
        app.chat_widget.active_collaboration_mode_kind(),
        ModeKind::Plan
    );
    assert_eq!(app.chat_widget.current_model(), "gpt-restored");
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn replayed_interrupted_turn_restores_queued_input_to_composer() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    let session_configured = Event {
        id: "session-configured".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    };
    app.chat_widget
        .handle_codex_event(session_configured.clone());
    app.chat_widget.handle_codex_event(Event {
        id: "turn-started".to_string(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
    });
    app.chat_widget.handle_codex_event(Event {
        id: "agent-delta".to_string(),
        msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
            process_id: String::new(),
            turn_id: String::new(),
            item_id: String::new(),
            delta: "streaming".to_string(),
        }),
    });
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_process_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_codex_event(session_configured);
    while new_op_rx.try_recv().is_ok() {}

    app.replay_process_snapshot(
        ProcessEventSnapshot {
            session_configured: None,
            events: vec![Event {
                id: "turn-aborted".to_string(),
                msg: EventMsg::TurnAborted(TurnAbortedEvent {
                    turn_id: Some("turn-1".to_string()),
                    reason: TurnAbortReason::ReviewEnded,
                }),
            }],
            input_state: Some(input_state),
        },
        true,
    );

    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "queued follow-up"
    );
    assert!(app.chat_widget.queued_user_message_texts().is_empty());
    assert!(
        new_op_rx.try_recv().is_err(),
        "replayed interrupted turns should restore queued input for editing, not submit it"
    );
}

#[tokio::test]
async fn live_turn_started_refreshes_status_line_with_runtime_context_window() {
    let mut app = make_test_app().await;
    app.chat_widget
        .setup_status_line(vec![crate::bottom_pane::StatusLineItem::ContextWindowSize]);

    assert_eq!(app.chat_widget.status_line_text(), None);

    app.handle_codex_event_now(Event {
        id: "turn-started".to_string(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            model_context_window: Some(950_000),
            collaboration_mode_kind: Default::default(),
        }),
    });

    assert_eq!(
        app.chat_widget.status_line_text(),
        Some("950K window".into())
    );
}

#[tokio::test]
async fn open_agent_picker_keeps_missing_threads_for_replay() -> Result<()> {
    let mut app = make_test_app().await;
    let process_id = ProcessId::new();
    app.process_event_channels
        .insert(process_id, ProcessEventChannel::new(1));

    app.open_agent_picker().await;

    assert_eq!(app.process_event_channels.contains_key(&process_id), true);
    assert_eq!(
        app.agent_navigation.get(&process_id),
        Some(&AgentPickerProcessEntry {
            agent_nickname: None,
            agent_role: None,
            is_closed: true,
        })
    );
    assert_eq!(app.agent_navigation.ordered_process_ids(), vec![process_id]);
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_keeps_cached_closed_processes() -> Result<()> {
    let mut app = make_test_app().await;
    let process_id = ProcessId::new();
    app.process_event_channels
        .insert(process_id, ProcessEventChannel::new(1));
    app.agent_navigation.upsert(
        process_id,
        Some("Robie".to_string()),
        Some("scout".to_string()),
        false,
    );

    app.open_agent_picker().await;

    assert_eq!(app.process_event_channels.contains_key(&process_id), true);
    assert_eq!(
        app.agent_navigation.get(&process_id),
        Some(&AgentPickerProcessEntry {
            agent_nickname: Some("Robie".to_string()),
            agent_role: Some("scout".to_string()),
            is_closed: true,
        })
    );
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_selects_existing_agent_process() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    app.process_event_channels
        .insert(process_id, ProcessEventChannel::new(1));

    app.open_agent_picker().await;
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_matches!(
        app_event_rx.try_recv(),
        Ok(AppEvent::SelectAgentProcess(selected_process_id)) if selected_process_id == process_id
    );
    Ok(())
}

#[tokio::test]
async fn refresh_pending_process_approvals_only_lists_inactive_processes() {
    let mut app = make_test_app().await;
    let main_process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000001").expect("valid thread");
    let agent_process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000002").expect("valid thread");

    app.primary_process_id = Some(main_process_id);
    app.active_process_id = Some(main_process_id);
    app.process_event_channels
        .insert(main_process_id, ProcessEventChannel::new(1));

    let agent_channel = ProcessEventChannel::new(1);
    {
        let mut store = agent_channel.store.lock().await;
        store.push_event(Event {
            id: "ev-1".to_string(),
            msg: EventMsg::ExecApprovalRequest(chaos_ipc::protocol::ExecApprovalRequestEvent {
                call_id: "call-1".to_string(),
                approval_id: None,
                turn_id: "turn-1".to_string(),
                command: vec!["echo".to_string(), "hi".to_string()],
                cwd: PathBuf::from("/tmp"),
                reason: None,
                network_approval_context: None,
                proposed_execpolicy_amendment: None,
                proposed_network_policy_amendments: None,
                additional_permissions: None,
                available_decisions: None,
                parsed_cmd: Vec::new(),
            }),
        });
    }
    app.process_event_channels
        .insert(agent_process_id, agent_channel);
    app.agent_navigation.upsert(
        agent_process_id,
        Some("Robie".to_string()),
        Some("scout".to_string()),
        false,
    );

    app.refresh_pending_process_approvals().await;
    assert_eq!(
        app.chat_widget.pending_process_approvals(),
        &["Robie [scout]".to_string()]
    );

    app.active_process_id = Some(agent_process_id);
    app.refresh_pending_process_approvals().await;
    assert!(app.chat_widget.pending_process_approvals().is_empty());
}

#[tokio::test]
async fn inactive_process_approval_bubbles_into_active_view() -> Result<()> {
    let mut app = make_test_app().await;
    let main_process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
    let agent_process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000022").expect("valid thread");

    app.primary_process_id = Some(main_process_id);
    app.active_process_id = Some(main_process_id);
    app.process_event_channels
        .insert(main_process_id, ProcessEventChannel::new(1));
    app.process_event_channels.insert(
        agent_process_id,
        ProcessEventChannel::new_with_session_configured(
            1,
            Event {
                id: String::new(),
                msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                    session_id: agent_process_id,
                    forked_from_id: None,
                    process_name: None,
                    model: "gpt-5".to_string(),
                    model_provider_id: "test-provider".to_string(),
                    service_tier: None,
                    approval_policy: ApprovalPolicy::Interactive,
                    approvals_reviewer: ApprovalsReviewer::User,
                    file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                        &SandboxPolicy::new_workspace_write_policy(),
                    ),
                    network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                        &SandboxPolicy::new_workspace_write_policy(),
                    ),
                    cwd: PathBuf::from("/tmp/agent"),
                    reasoning_effort: None,
                    history_log_id: 0,
                    history_entry_count: 0,
                    initial_messages: None,
                    network_proxy: None,
                }),
            },
        ),
    );
    app.agent_navigation.upsert(
        agent_process_id,
        Some("Robie".to_string()),
        Some("scout".to_string()),
        false,
    );

    app.enqueue_process_event(
        agent_process_id,
        Event {
            id: "ev-approval".to_string(),
            msg: EventMsg::ExecApprovalRequest(chaos_ipc::protocol::ExecApprovalRequestEvent {
                call_id: "call-approval".to_string(),
                approval_id: None,
                turn_id: "turn-approval".to_string(),
                command: vec!["echo".to_string(), "hi".to_string()],
                cwd: PathBuf::from("/tmp/agent"),
                reason: Some("need approval".to_string()),
                network_approval_context: None,
                proposed_execpolicy_amendment: None,
                proposed_network_policy_amendments: None,
                additional_permissions: None,
                available_decisions: None,
                parsed_cmd: Vec::new(),
            }),
        },
    )
    .await?;

    assert_eq!(app.chat_widget.has_active_view(), true);
    assert_eq!(
        app.chat_widget.pending_process_approvals(),
        &["Robie [scout]".to_string()]
    );

    Ok(())
}

#[test]
fn agent_picker_item_name_snapshot() {
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id");
    let snapshot = [
        format!(
            "{} | {}",
            format_agent_picker_item_name(Some("Robie"), Some("scout"), true),
            process_id
        ),
        format!(
            "{} | {}",
            format_agent_picker_item_name(Some("Robie"), Some("scout"), false),
            process_id
        ),
        format!(
            "{} | {}",
            format_agent_picker_item_name(Some("Robie"), None, false),
            process_id
        ),
        format!(
            "{} | {}",
            format_agent_picker_item_name(None, Some("scout"), false),
            process_id
        ),
        format!(
            "{} | {}",
            format_agent_picker_item_name(None, None, false),
            process_id
        ),
    ]
    .join("\n");
    assert_snapshot!("agent_picker_item_name", snapshot);
}

#[tokio::test]
async fn active_non_primary_shutdown_target_returns_none_for_non_shutdown_event() -> Result<()> {
    let mut app = make_test_app().await;
    app.active_process_id = Some(ProcessId::new());
    app.primary_process_id = Some(ProcessId::new());

    assert_eq!(
        app.active_non_primary_shutdown_target(&EventMsg::ListCustomPromptsResponse(
            chaos_ipc::protocol::ListCustomPromptsResponseEvent {
                custom_prompts: vec![]
            }
        )),
        None
    );
    Ok(())
}

#[tokio::test]
async fn active_non_primary_shutdown_target_returns_none_for_primary_thread_shutdown() -> Result<()>
{
    let mut app = make_test_app().await;
    let process_id = ProcessId::new();
    app.active_process_id = Some(process_id);
    app.primary_process_id = Some(process_id);

    assert_eq!(
        app.active_non_primary_shutdown_target(&EventMsg::ShutdownComplete),
        None
    );
    Ok(())
}

#[tokio::test]
async fn active_non_primary_shutdown_target_returns_ids_for_non_primary_shutdown() -> Result<()> {
    let mut app = make_test_app().await;
    let active_process_id = ProcessId::new();
    let primary_process_id = ProcessId::new();
    app.active_process_id = Some(active_process_id);
    app.primary_process_id = Some(primary_process_id);

    assert_eq!(
        app.active_non_primary_shutdown_target(&EventMsg::ShutdownComplete),
        Some((active_process_id, primary_process_id))
    );
    Ok(())
}

#[tokio::test]
async fn active_non_primary_shutdown_target_returns_none_when_shutdown_exit_is_pending()
-> Result<()> {
    let mut app = make_test_app().await;
    let active_process_id = ProcessId::new();
    let primary_process_id = ProcessId::new();
    app.active_process_id = Some(active_process_id);
    app.primary_process_id = Some(primary_process_id);
    app.pending_shutdown_exit_process_id = Some(active_process_id);

    assert_eq!(
        app.active_non_primary_shutdown_target(&EventMsg::ShutdownComplete),
        None
    );
    Ok(())
}

#[tokio::test]
async fn active_non_primary_shutdown_target_still_switches_for_other_pending_exit_thread()
-> Result<()> {
    let mut app = make_test_app().await;
    let active_process_id = ProcessId::new();
    let primary_process_id = ProcessId::new();
    app.active_process_id = Some(active_process_id);
    app.primary_process_id = Some(primary_process_id);
    app.pending_shutdown_exit_process_id = Some(ProcessId::new());

    assert_eq!(
        app.active_non_primary_shutdown_target(&EventMsg::ShutdownComplete),
        Some((active_process_id, primary_process_id))
    );
    Ok(())
}

async fn render_clear_ui_header_after_long_transcript_for_snapshot() -> String {
    let mut app = make_test_app().await;
    app.config.cwd = PathBuf::from("/tmp/project");
    app.chat_widget.set_model("gpt-test");
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    let story_part_one = "In the cliffside town of Bracken Ferry, the lighthouse had been dark for \
        nineteen years, and the children were told it was because the sea no longer wanted a \
        guide. Mara, who repaired clocks for a living, found that hard to believe. Every dawn she \
        heard the gulls circling the empty tower, and every dusk she watched ships hesitate at the \
        mouth of the bay as if listening for a signal that never came. When an old brass key fell \
        out of a cracked parcel in her workshop, tagged only with the words 'for the lamp room,' \
        she decided to climb the hill and see what the town had forgotten.";
    let story_part_two = "Inside the lighthouse she found gears wrapped in oilcloth, logbooks filled \
        with weather notes, and a lens shrouded beneath salt-stiff canvas. The mechanism was not \
        broken, only unfinished. Someone had removed the governor spring and hidden it in a false \
        drawer, along with a letter from the last keeper admitting he had darkened the light on \
        purpose after smugglers threatened his family. Mara spent the night rebuilding the clockwork \
        from spare watch parts, her fingers blackened with soot and grease, while a storm gathered \
        over the water and the harbor bells began to ring.";
    let story_part_three = "At midnight the first squall hit, and the fishing boats returned early, \
        blind in sheets of rain. Mara wound the mechanism, set the teeth by hand, and watched the \
        great lens begin to turn in slow, certain arcs. The beam swept across the bay, caught the \
        whitecaps, and reached the boats just as they were drifting toward the rocks below the \
        eastern cliffs. In the morning the town square was crowded with wet sailors, angry elders, \
        and wide-eyed children, but when the oldest captain placed the keeper's log on the fountain \
        and thanked Mara for relighting the coast, nobody argued. By sunset, Bracken Ferry had a \
        lighthouse again, and Mara had more clocks to mend than ever because everyone wanted \
        something in town to keep better time.";

    let user_cell = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(UserHistoryCell {
            message: text.to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>
    };
    let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(AgentMessageCell::new(
            vec![Line::from(text.to_string())],
            true,
        )) as Arc<dyn HistoryCell>
    };
    let make_header = |is_first| -> Arc<dyn HistoryCell> {
        let event = SessionConfiguredEvent {
            session_id: ProcessId::new(),
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: Some(ReasoningEffortConfig::High),
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        };
        Arc::new(new_session_info(
            app.chat_widget.config_ref(),
            app.chat_widget.current_model(),
            event,
            is_first,
        )) as Arc<dyn HistoryCell>
    };

    app.transcript_cells = vec![
        make_header(true),
        Arc::new(crate::history_cell::new_info_event(
            "startup tip that used to replay".to_string(),
            None,
        )) as Arc<dyn HistoryCell>,
        user_cell("Tell me a long story about a town with a dark lighthouse."),
        agent_cell(story_part_one),
        user_cell("Continue the story and reveal why the light went out."),
        agent_cell(story_part_two),
        user_cell("Finish the story with a storm and a resolution."),
        agent_cell(story_part_three),
    ];
    app.has_emitted_history_lines = true;

    let rendered = app
        .clear_ui_header_lines(80)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !rendered.contains("startup tip that used to replay"),
        "clear header should not replay startup notices"
    );
    assert!(
        !rendered.contains("Bracken Ferry"),
        "clear header should not replay prior conversation turns"
    );
    rendered
}

#[tokio::test]
async fn clear_ui_after_long_transcript_snapshots_fresh_header_only() {
    let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
    assert_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
}

#[tokio::test]
async fn ctrl_l_clear_ui_after_long_transcript_reuses_clear_header_snapshot() {
    let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
    assert_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
}

async fn make_test_app() -> App {
    let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    let config = chat_widget.config_ref().clone();
    let server = Arc::new(
        chaos_kern::test_support::process_table_with_models_provider(
            ChaosAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ),
    );
    let auth_manager =
        chaos_kern::test_support::auth_manager_from_auth(ChaosAuth::from_api_key("Test API Key"));
    let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
    let model = chaos_kern::test_support::get_model_offline(config.model.as_deref());
    let session_telemetry = test_session_telemetry(&config, model.as_str());
    let tool_list_pane = Rc::new(RefCell::new(ToolListPane::new()));
    let tool_list_close = Rc::new(Cell::new(false));

    App {
        server,
        session_telemetry,
        app_event_tx,
        chat_widget,
        auth_manager,
        config,
        active_profile: None,
        cli_kv_overrides: Vec::new(),
        harness_overrides: ConfigOverrides::default(),
        runtime_approval_policy_override: None,
        runtime_sandbox_policy_override: None,
        tile_manager: TileManager::new(tool_list_pane.clone(), tool_list_close.clone()),
        tool_list_pane,
        tool_list_close,
        file_search,
        log_state_db: None,
        log_state_db_init_error: None,
        log_panel: LogPanelState::default(),
        transcript_cells: Vec::new(),
        overlay: None,
        deferred_history_lines: Vec::new(),
        has_emitted_history_lines: false,
        enhanced_keys_supported: false,
        commit_anim_running: Arc::new(AtomicBool::new(false)),
        status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        backtrack: BacktrackState::default(),
        backtrack_render_pending: false,
        suppress_shutdown_complete: false,
        pending_shutdown_exit_process_id: None,

        process_event_channels: HashMap::new(),
        process_event_listener_tasks: HashMap::new(),
        agent_navigation: AgentNavigationState::default(),
        active_process_id: None,
        active_process_rx: None,
        primary_process_id: None,
        primary_session_configured: None,
        pending_primary_events: VecDeque::new(),
    }
}

#[cfg(feature = "vt100-tests")]
fn make_test_tui() -> crate::tui::Tui {
    use ratatui::backend::CrosstermBackend;
    let backend = CrosstermBackend::new(std::io::stdout());
    let terminal = crate::custom_terminal::Terminal::new_for_test(backend, 100, 30);
    let mut tui = crate::tui::Tui::new(terminal);
    tui.set_alt_screen_enabled(false);
    tui
}

async fn make_test_app_with_channels() -> (
    App,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (chat_widget, app_event_tx, rx, op_rx) = make_chatwidget_manual_with_sender().await;
    let config = chat_widget.config_ref().clone();
    let server = Arc::new(
        chaos_kern::test_support::process_table_with_models_provider(
            ChaosAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ),
    );
    let auth_manager =
        chaos_kern::test_support::auth_manager_from_auth(ChaosAuth::from_api_key("Test API Key"));
    let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
    let model = chaos_kern::test_support::get_model_offline(config.model.as_deref());
    let session_telemetry = test_session_telemetry(&config, model.as_str());
    let tool_list_pane = Rc::new(RefCell::new(ToolListPane::new()));
    let tool_list_close = Rc::new(Cell::new(false));

    (
        App {
            server,
            session_telemetry,
            app_event_tx,
            chat_widget,
            auth_manager,
            config,
            active_profile: None,
            cli_kv_overrides: Vec::new(),
            harness_overrides: ConfigOverrides::default(),
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            tile_manager: TileManager::new(tool_list_pane.clone(), tool_list_close.clone()),
            tool_list_pane,
            tool_list_close,
            file_search,
            log_state_db: None,
            log_state_db_init_error: None,
            log_panel: LogPanelState::default(),
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            enhanced_keys_supported: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            suppress_shutdown_complete: false,
            pending_shutdown_exit_process_id: None,

            process_event_channels: HashMap::new(),
            process_event_listener_tasks: HashMap::new(),
            agent_navigation: AgentNavigationState::default(),
            active_process_id: None,
            active_process_rx: None,
            primary_process_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
        },
        rx,
        op_rx,
    )
}

fn next_user_turn_op(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> Op {
    let mut seen = Vec::new();
    while let Ok(op) = op_rx.try_recv() {
        if matches!(op, Op::UserTurn { .. }) {
            return op;
        }
        seen.push(format!("{op:?}"));
    }
    panic!("expected UserTurn op, saw: {seen:?}");
}

fn test_session_telemetry(config: &Config, model: &str) -> SessionTelemetry {
    let model_info = chaos_kern::test_support::construct_model_info_offline(model, config);
    SessionTelemetry::new(
        ProcessId::new(),
        model,
        model_info.slug.as_str(),
        None,
        "test_originator".to_string(),
        false,
        "test".to_string(),
        SessionSource::Cli,
    )
}

fn app_enabled_in_effective_config(config: &Config, app_id: &str) -> Option<bool> {
    config
        .config_layer_stack
        .effective_config()
        .as_table()
        .and_then(|table| table.get("apps"))
        .and_then(TomlValue::as_table)
        .and_then(|apps| apps.get(app_id))
        .and_then(TomlValue::as_table)
        .and_then(|app| app.get("enabled"))
        .and_then(TomlValue::as_bool)
}

#[tokio::test]
async fn update_reasoning_effort_updates_collaboration_mode() {
    let mut app = make_test_app().await;
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

    app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        app.config.model_reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn refresh_in_memory_config_from_disk_loads_latest_apps_state() -> Result<()> {
    let mut app = make_test_app().await;
    let chaos_home = tempdir()?;
    app.config.chaos_home = chaos_home.path().to_path_buf();
    let app_id = "unit_test_refresh_in_memory_config_connector".to_string();

    assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

    ConfigEditsBuilder::new(&app.config.chaos_home)
        .with_edits([
            ConfigEdit::SetPath {
                segments: vec!["apps".to_string(), app_id.clone(), "enabled".to_string()],
                value: false.into(),
            },
            ConfigEdit::SetPath {
                segments: vec![
                    "apps".to_string(),
                    app_id.clone(),
                    "disabled_reason".to_string(),
                ],
                value: "user".into(),
            },
        ])
        .apply()
        .await
        .expect("persist app toggle");

    assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

    app.refresh_in_memory_config_from_disk().await?;

    assert_eq!(
        app_enabled_in_effective_config(&app.config, &app_id),
        Some(false)
    );
    Ok(())
}

#[tokio::test]
async fn refresh_in_memory_config_from_disk_best_effort_keeps_current_config_on_error() -> Result<()>
{
    let mut app = make_test_app().await;
    let chaos_home = tempdir()?;
    app.config.chaos_home = chaos_home.path().to_path_buf();
    std::fs::write(chaos_home.path().join("config.toml"), "[broken")?;
    let original_config = app.config.clone();

    app.refresh_in_memory_config_from_disk_best_effort("starting a new thread")
        .await;

    assert_eq!(app.config, original_config);
    Ok(())
}

#[tokio::test]
async fn refresh_in_memory_config_from_disk_uses_active_chat_widget_cwd() -> Result<()> {
    let mut app = make_test_app().await;
    let original_cwd = app.config.cwd.clone();
    let next_cwd_tmp = tempdir()?;
    let next_cwd = next_cwd_tmp.path().to_path_buf();

    app.chat_widget.handle_codex_event(Event {
        id: String::new(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: ProcessId::new(),
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: next_cwd.clone(),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    });

    assert_eq!(app.chat_widget.config_ref().cwd, next_cwd);
    assert_eq!(app.config.cwd, original_cwd);

    app.refresh_in_memory_config_from_disk().await?;

    assert_eq!(app.config.cwd, app.chat_widget.config_ref().cwd);
    Ok(())
}

#[tokio::test]
async fn rebuild_config_for_resume_or_fallback_uses_current_config_on_same_cwd_error() -> Result<()>
{
    let mut app = make_test_app().await;
    let chaos_home = tempdir()?;
    app.config.chaos_home = chaos_home.path().to_path_buf();
    std::fs::write(chaos_home.path().join("config.toml"), "[broken")?;
    let current_config = app.config.clone();
    let current_cwd = current_config.cwd.clone();

    let resume_config = app
        .rebuild_config_for_resume_or_fallback(&current_cwd, current_cwd.clone())
        .await?;

    assert_eq!(resume_config, current_config);
    Ok(())
}

#[tokio::test]
async fn rebuild_config_for_resume_or_fallback_errors_when_cwd_changes() -> Result<()> {
    let mut app = make_test_app().await;
    let chaos_home = tempdir()?;
    app.config.chaos_home = chaos_home.path().to_path_buf();
    std::fs::write(chaos_home.path().join("config.toml"), "[broken")?;
    let current_cwd = app.config.cwd.clone();
    let next_cwd_tmp = tempdir()?;
    let next_cwd = next_cwd_tmp.path().to_path_buf();

    let result = app
        .rebuild_config_for_resume_or_fallback(&current_cwd, next_cwd)
        .await;

    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn sync_tui_theme_selection_updates_chat_widget_config_copy() {
    let mut app = make_test_app().await;

    app.sync_tui_theme_selection("dracula".to_string());

    assert_eq!(app.config.tui_theme.as_deref(), Some("dracula"));
    assert_eq!(
        app.chat_widget.config_ref().tui_theme.as_deref(),
        Some("dracula")
    );
}

#[tokio::test]
async fn backtrack_selection_with_duplicate_history_targets_unique_turn() {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

    let user_cell = |text: &str,
                     text_elements: Vec<TextElement>,
                     local_image_paths: Vec<PathBuf>,
                     remote_image_urls: Vec<String>|
     -> Arc<dyn HistoryCell> {
        Arc::new(UserHistoryCell {
            message: text.to_string(),
            text_elements,
            local_image_paths,
            remote_image_urls,
        }) as Arc<dyn HistoryCell>
    };
    let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(AgentMessageCell::new(
            vec![Line::from(text.to_string())],
            true,
        )) as Arc<dyn HistoryCell>
    };

    let make_header = |is_first| {
        let event = SessionConfiguredEvent {
            session_id: ProcessId::new(),
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        };
        Arc::new(new_session_info(
            app.chat_widget.config_ref(),
            app.chat_widget.current_model(),
            event,
            is_first,
        )) as Arc<dyn HistoryCell>
    };

    let placeholder = "[Image #1]";
    let edited_text = format!("follow-up (edited) {placeholder}");
    let edited_range = edited_text.len().saturating_sub(placeholder.len())..edited_text.len();
    let edited_text_elements = vec![TextElement::new(edited_range.into(), None)];
    let edited_local_image_paths = vec![PathBuf::from("/tmp/fake-image.png")];

    // Simulate a transcript with duplicated history (e.g., from prior backtracks)
    // and an edited turn appended after a session header boundary.
    app.transcript_cells = vec![
        make_header(true),
        user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
        agent_cell("answer first"),
        user_cell("follow-up", Vec::new(), Vec::new(), Vec::new()),
        agent_cell("answer follow-up"),
        make_header(false),
        user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
        agent_cell("answer first"),
        user_cell(
            &edited_text,
            edited_text_elements.clone(),
            edited_local_image_paths.clone(),
            vec!["https://example.com/backtrack.png".to_string()],
        ),
        agent_cell("answer edited"),
    ];

    assert_eq!(user_count(&app.transcript_cells), 2);

    let base_id = ProcessId::new();
    app.chat_widget.handle_codex_event(Event {
        id: String::new(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: base_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    });

    app.backtrack.base_id = Some(base_id);
    app.backtrack.primed = true;
    app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

    let selection = app
        .confirm_backtrack_from_main()
        .expect("backtrack selection");
    assert_eq!(selection.nth_user_message, 1);
    assert_eq!(selection.prefill, edited_text);
    assert_eq!(selection.text_elements, edited_text_elements);
    assert_eq!(selection.local_image_paths, edited_local_image_paths);
    assert_eq!(
        selection.remote_image_urls,
        vec!["https://example.com/backtrack.png".to_string()]
    );

    app.apply_backtrack_rollback(selection);
    assert_eq!(
        app.chat_widget.remote_image_urls(),
        vec!["https://example.com/backtrack.png".to_string()]
    );

    let mut rollback_turns = None;
    while let Ok(op) = op_rx.try_recv() {
        if let Op::ProcessRollback { num_turns } = op {
            rollback_turns = Some(num_turns);
        }
    }

    assert_eq!(rollback_turns, Some(1));
}

#[tokio::test]
async fn backtrack_remote_image_only_selection_clears_existing_composer_draft() {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

    app.transcript_cells = vec![Arc::new(UserHistoryCell {
        message: "original".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    }) as Arc<dyn HistoryCell>];
    app.chat_widget
        .set_composer_text("stale draft".to_string(), Vec::new(), Vec::new());

    let remote_image_url = "https://example.com/remote-only.png".to_string();
    app.apply_backtrack_rollback(BacktrackSelection {
        nth_user_message: 0,
        prefill: String::new(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: vec![remote_image_url.clone()],
    });

    assert_eq!(app.chat_widget.composer_text_with_pending(), "");
    assert_eq!(app.chat_widget.remote_image_urls(), vec![remote_image_url]);

    let mut rollback_turns = None;
    while let Ok(op) = op_rx.try_recv() {
        if let Op::ProcessRollback { num_turns } = op {
            rollback_turns = Some(num_turns);
        }
    }
    assert_eq!(rollback_turns, Some(1));
}

#[tokio::test]
async fn backtrack_resubmit_preserves_data_image_urls_in_user_turn() {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

    let process_id = ProcessId::new();
    app.chat_widget.handle_codex_event(Event {
        id: String::new(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    });

    let data_image_url = "data:image/png;base64,abc123".to_string();
    app.transcript_cells = vec![Arc::new(UserHistoryCell {
        message: "please inspect this".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: vec![data_image_url.clone()],
    }) as Arc<dyn HistoryCell>];

    app.apply_backtrack_rollback(BacktrackSelection {
        nth_user_message: 0,
        prefill: "please inspect this".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: vec![data_image_url.clone()],
    });

    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let mut saw_rollback = false;
    let mut submitted_items: Option<Vec<UserInput>> = None;
    while let Ok(op) = op_rx.try_recv() {
        match op {
            Op::ProcessRollback { .. } => saw_rollback = true,
            Op::UserTurn { items, .. } => submitted_items = Some(items),
            _ => {}
        }
    }

    assert!(saw_rollback);
    let items = submitted_items.expect("expected user turn after backtrack resubmit");
    assert!(items.iter().any(|item| {
        matches!(
            item,
            UserInput::Image { image_url } if image_url == &data_image_url
        )
    }));
}

#[tokio::test]
async fn replayed_initial_messages_apply_rollback_in_queue_order() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

    let session_id = ProcessId::new();
    app.handle_codex_event_replay(Event {
        id: String::new(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: Some(vec![
                EventMsg::UserMessage(UserMessageEvent {
                    message: "first prompt".to_string(),
                    images: None,
                    local_images: Vec::new(),
                    text_elements: Vec::new(),
                }),
                EventMsg::UserMessage(UserMessageEvent {
                    message: "second prompt".to_string(),
                    images: None,
                    local_images: Vec::new(),
                    text_elements: Vec::new(),
                }),
                EventMsg::ProcessRolledBack(ProcessRolledBackEvent { num_turns: 1 }),
                EventMsg::UserMessage(UserMessageEvent {
                    message: "third prompt".to_string(),
                    images: None,
                    local_images: Vec::new(),
                    text_elements: Vec::new(),
                }),
            ]),
            network_proxy: None,
        }),
    });

    let mut saw_rollback = false;
    while let Ok(event) = app_event_rx.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(cell) => {
                let cell: Arc<dyn HistoryCell> = cell.into();
                app.transcript_cells.push(cell);
            }
            AppEvent::ApplyProcessRollback { num_turns } => {
                saw_rollback = true;
                crate::app_backtrack::trim_transcript_cells_drop_last_n_user_turns(
                    &mut app.transcript_cells,
                    num_turns,
                );
            }
            _ => {}
        }
    }

    assert!(saw_rollback);
    let user_messages: Vec<String> = app
        .transcript_cells
        .iter()
        .filter_map(|cell| {
            cell.as_any()
                .downcast_ref::<UserHistoryCell>()
                .map(|cell| cell.message.clone())
        })
        .collect();
    assert_eq!(
        user_messages,
        vec!["first prompt".to_string(), "third prompt".to_string()]
    );
}

#[tokio::test]
async fn live_rollback_during_replay_is_applied_in_app_event_order() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

    let session_id = ProcessId::new();
    app.handle_codex_event_replay(Event {
        id: String::new(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: Some(vec![
                EventMsg::UserMessage(UserMessageEvent {
                    message: "first prompt".to_string(),
                    images: None,
                    local_images: Vec::new(),
                    text_elements: Vec::new(),
                }),
                EventMsg::UserMessage(UserMessageEvent {
                    message: "second prompt".to_string(),
                    images: None,
                    local_images: Vec::new(),
                    text_elements: Vec::new(),
                }),
            ]),
            network_proxy: None,
        }),
    });

    // Simulate a live rollback arriving before queued replay inserts are drained.
    app.handle_codex_event_now(Event {
        id: "live-rollback".to_string(),
        msg: EventMsg::ProcessRolledBack(ProcessRolledBackEvent { num_turns: 1 }),
    });

    let mut saw_rollback = false;
    while let Ok(event) = app_event_rx.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(cell) => {
                let cell: Arc<dyn HistoryCell> = cell.into();
                app.transcript_cells.push(cell);
            }
            AppEvent::ApplyProcessRollback { num_turns } => {
                saw_rollback = true;
                crate::app_backtrack::trim_transcript_cells_drop_last_n_user_turns(
                    &mut app.transcript_cells,
                    num_turns,
                );
            }
            _ => {}
        }
    }

    assert!(saw_rollback);
    let user_messages: Vec<String> = app
        .transcript_cells
        .iter()
        .filter_map(|cell| {
            cell.as_any()
                .downcast_ref::<UserHistoryCell>()
                .map(|cell| cell.message.clone())
        })
        .collect();
    assert_eq!(user_messages, vec!["first prompt".to_string()]);
}

#[tokio::test]
async fn queued_rollback_syncs_overlay_and_clears_deferred_history() {
    let mut app = make_test_app().await;
    app.transcript_cells = vec![
        Arc::new(UserHistoryCell {
            message: "first".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>,
        Arc::new(AgentMessageCell::new(
            vec![Line::from("after first")],
            false,
        )) as Arc<dyn HistoryCell>,
        Arc::new(UserHistoryCell {
            message: "second".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>,
        Arc::new(AgentMessageCell::new(
            vec![Line::from("after second")],
            false,
        )) as Arc<dyn HistoryCell>,
    ];
    app.overlay = Some(Overlay::new_transcript(app.transcript_cells.clone()));
    app.deferred_history_lines = vec![Line::from("stale buffered line")];
    app.backtrack.overlay_preview_active = true;
    app.backtrack.nth_user_message = 1;

    let changed = app.apply_non_pending_process_rollback(1);

    assert!(changed);
    assert!(app.backtrack_render_pending);
    assert!(app.deferred_history_lines.is_empty());
    assert_eq!(app.backtrack.nth_user_message, 0);
    let user_messages: Vec<String> = app
        .transcript_cells
        .iter()
        .filter_map(|cell| {
            cell.as_any()
                .downcast_ref::<UserHistoryCell>()
                .map(|cell| cell.message.clone())
        })
        .collect();
    assert_eq!(user_messages, vec!["first".to_string()]);
    let overlay_cell_count = match app.overlay.as_ref() {
        Some(Overlay::Transcript(t)) => t.committed_cell_count(),
        _ => panic!("expected transcript overlay"),
    };
    assert_eq!(overlay_cell_count, app.transcript_cells.len());
}

#[cfg(feature = "vt100-tests")]
#[tokio::test]
async fn page_up_opens_transcript_overlay_from_main_view() {
    let mut app = make_test_app().await;
    let mut tui = make_test_tui();
    app.transcript_cells = vec![
        Arc::new(UserHistoryCell {
            message: "first".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>,
        Arc::new(AgentMessageCell::new(vec![Line::from("reply")], false)) as Arc<dyn HistoryCell>,
    ];

    app.handle_key_event(&mut tui, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        .await;

    assert!(matches!(app.overlay, Some(Overlay::Transcript(_))));
}

#[cfg(feature = "vt100-tests")]
#[tokio::test]
async fn page_up_keeps_log_panel_priority_when_visible() {
    let mut app = make_test_app().await;
    let mut tui = make_test_tui();
    app.log_panel = LogPanelState::default();
    app.overlay = Some(Overlay::new_static_with_lines(
        vec![],
        "L O G S".to_string(),
    ));

    app.handle_key_event(&mut tui, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        .await;

    assert!(
        !matches!(app.overlay, Some(Overlay::Transcript(_))),
        "log panel PageUp should not open transcript overlay"
    );
}

#[tokio::test]
async fn new_session_requests_shutdown_for_previous_conversation() {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;

    let process_id = ProcessId::new();
    let event = SessionConfiguredEvent {
        session_id: process_id,
        forked_from_id: None,
        process_name: None,
        model: "gpt-test".to_string(),
        model_provider_id: "test-provider".to_string(),
        service_tier: None,
        approval_policy: ApprovalPolicy::Headless,
        approvals_reviewer: ApprovalsReviewer::User,
        file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
            &SandboxPolicy::new_read_only_policy(),
        ),
        network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
            &SandboxPolicy::new_read_only_policy(),
        ),
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: None,
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        network_proxy: None,
    };

    app.chat_widget.handle_codex_event(Event {
        id: String::new(),
        msg: EventMsg::SessionConfigured(event),
    });

    while app_event_rx.try_recv().is_ok() {}
    while op_rx.try_recv().is_ok() {}

    app.shutdown_current_process().await;

    match op_rx.try_recv() {
        Ok(Op::Shutdown) => {}
        Ok(other) => panic!("expected Op::Shutdown, got {other:?}"),
        Err(_) => panic!("expected shutdown op to be sent"),
    }
}

#[tokio::test]
async fn shutdown_first_exit_returns_immediate_exit_when_shutdown_submit_fails() {
    let mut app = make_test_app().await;
    let process_id = ProcessId::new();
    app.active_process_id = Some(process_id);

    let control = app.handle_exit_mode(ExitMode::ShutdownFirst);

    assert_eq!(app.pending_shutdown_exit_process_id, None);
    assert!(matches!(
        control,
        AppRunControl::Exit(ExitReason::UserRequested)
    ));
}

#[tokio::test]
async fn shutdown_first_exit_waits_for_shutdown_when_submit_succeeds() {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let process_id = ProcessId::new();
    app.active_process_id = Some(process_id);

    let control = app.handle_exit_mode(ExitMode::ShutdownFirst);

    assert_eq!(app.pending_shutdown_exit_process_id, Some(process_id));
    assert!(matches!(control, AppRunControl::Continue));
    assert_eq!(op_rx.try_recv(), Ok(Op::Shutdown));
}

#[tokio::test]
async fn clear_only_ui_reset_preserves_chat_session_state() {
    let mut app = make_test_app().await;
    let process_id = ProcessId::new();
    app.chat_widget.handle_codex_event(Event {
        id: String::new(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: Some("keep me".to_string()),
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            file_system_sandbox_policy: chaos_ipc::protocol::FileSystemSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            network_sandbox_policy: chaos_ipc::protocol::NetworkSandboxPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/tmp/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    });
    app.chat_widget
        .apply_external_edit("draft prompt".to_string());
    app.transcript_cells = vec![Arc::new(UserHistoryCell {
        message: "old message".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    }) as Arc<dyn HistoryCell>];
    app.overlay = Some(Overlay::new_transcript(app.transcript_cells.clone()));
    app.deferred_history_lines = vec![Line::from("stale buffered line")];
    app.has_emitted_history_lines = true;
    app.backtrack.primed = true;
    app.backtrack.overlay_preview_active = true;
    app.backtrack.nth_user_message = 0;
    app.backtrack_render_pending = true;

    app.reset_app_ui_state_after_clear();

    assert!(app.overlay.is_none());
    assert!(app.transcript_cells.is_empty());
    assert!(app.deferred_history_lines.is_empty());
    assert!(!app.has_emitted_history_lines);
    assert!(!app.backtrack.primed);
    assert!(!app.backtrack.overlay_preview_active);
    assert!(app.backtrack.pending_rollback.is_none());
    assert!(!app.backtrack_render_pending);
    assert_eq!(app.chat_widget.process_id(), Some(process_id));
    assert_eq!(app.chat_widget.composer_text_with_pending(), "draft prompt");
}

#[tokio::test]
async fn session_summary_skip_zero_usage() {
    assert!(session_summary(TokenUsage::default(), None, None).is_none());
}

#[tokio::test]
async fn session_summary_includes_resume_hint() {
    let usage = TokenUsage {
        input_tokens: 10,
        output_tokens: 2,
        total_tokens: 12,
        ..Default::default()
    };
    let conversation = ProcessId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

    let summary = session_summary(usage, Some(conversation), None).expect("summary");
    assert_eq!(
        summary.usage_line,
        "Token usage: total=12 input=10 output=2"
    );
    assert_eq!(
        summary.resume_command,
        Some("chaos resume 123e4567-e89b-12d3-a456-426614174000".to_string())
    );
}

#[tokio::test]
async fn session_summary_prefers_name_over_id() {
    let usage = TokenUsage {
        input_tokens: 10,
        output_tokens: 2,
        total_tokens: 12,
        ..Default::default()
    };
    let conversation = ProcessId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

    let summary = session_summary(usage, Some(conversation), Some("my-session".to_string()))
        .expect("summary");
    assert_eq!(
        summary.resume_command,
        Some("chaos resume my-session".to_string())
    );
}
