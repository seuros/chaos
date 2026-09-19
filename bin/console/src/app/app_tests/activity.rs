use super::*;
use libui::activity::Phase;
use pretty_assertions::assert_eq;

#[test]
fn background_activity_survives_replay_and_resets_with_the_session() {
    std::thread::Builder::new()
        .name("console-activity-test".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(check_background_activity());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn check_background_activity() {
    let mut app = make_test_app().await;
    let parent = ProcessId::new();
    let child = ProcessId::new();
    app.enqueue_primary_event(session_configured_event(parent))
        .await
        .unwrap();
    app.ensure_process_channel(child);
    let started = Event {
        id: "child-turn".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "child-turn".into(),
            model_context_window: None,
            collaboration_mode_kind: ModeKind::Default,
        }),
    };
    app.handle_routed_process_event(child, started.clone())
        .await
        .unwrap();
    assert_eq!(app.active_process_id, Some(parent));
    assert_eq!(app.activity.get(parent).phase, Phase::Idle);
    assert_eq!(app.activity.get(child).phase, Phase::Working);
    let recorded = app.activity.get(child);
    let snapshot = app.process_event_channels[&child]
        .store
        .lock()
        .await
        .snapshot();
    app.replay_process_snapshot(snapshot, false);
    assert_eq!(
        app.activity.get(child),
        recorded,
        "replay cannot renew activity"
    );
    app.handle_routed_process_event(
        child,
        Event {
            id: "child-turn".into(),
            msg: EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: "child-turn".into(),
                last_agent_message: None,
            }),
        },
    )
    .await
    .unwrap();
    assert_eq!(app.activity.get(child).phase, Phase::Idle);
    app.reset_process_event_state();
    app.handle_routed_process_event(child, started)
        .await
        .unwrap();
    assert_eq!(
        app.activity.snapshot(None, false),
        Default::default(),
        "late events from a reset session must not resurrect activity"
    );
}
