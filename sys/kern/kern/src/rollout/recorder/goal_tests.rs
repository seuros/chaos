use super::*;
use chaos_mcp_protocol::goals::{Snapshot, Status, VERSION};
use serde_json::json;

#[tokio::test]
async fn goal_checkpoint_waits_for_confirm_and_reconciles_a_failed_ack() {
    let (session, mut turn, _) = crate::chaos::make_session_and_context_with_rx().await;
    let config = Arc::make_mut(&mut Arc::get_mut(&mut turn).unwrap().config);
    let (_, settings) = crate::reflex::configuration::presets()
        .into_iter()
        .next()
        .unwrap();
    config.reflex = std::collections::BTreeMap::from([("test".into(), settings)]);
    session
        .state
        .lock()
        .await
        .goal
        .begin_turn(&turn.sub_id, "fix");
    let (tx, mut rx) = mpsc::unbounded_channel();
    *session.services.rollout.lock().await = Some(RolloutRecorder {
        tx,
        runtime_db: None,
        event_persistence_mode: EventPersistenceMode::Limited,
        persist_acp_user_items: false,
        live_rollout_items: Arc::default(),
        writer_status: watch::channel(JournalWriterStatus::Ready).1,
    });
    crate::goal::request(
        &session,
        "driver".into(),
        "stdio:test".into(),
        1,
        json!({"version":VERSION,"operation":"restore"}),
    )
    .await
    .unwrap();
    let args = json!({"objective":"fix","criteria":["test passes"]});
    crate::goal::authorize(&session, &turn, "driver", "start_goal", args).await;
    let goal = Snapshot {
        id: "goal".into(),
        turn_id: turn.sub_id.clone(),
        revision: 1,
        objective: "fix".into(),
        criteria: vec!["test passes".into()],
        status: Status::Active,
        checks: 0,
        reason: "awaiting_checkpoint".into(),
        attempt_id: None,
        evidence_digest: None,
        verdict_ref: None,
    };
    let request =
        json!({"version":VERSION,"operation":"checkpoint","expected_revision":0,"goal":goal});
    let sess = Arc::clone(&session);
    let first = tokio::spawn(async move {
        crate::goal::request(&sess, "driver".into(), "stdio:test".into(), 1, request).await
    });
    let Some(RolloutCmd::AddItems(items)) = rx.recv().await else {
        panic!("expected structural checkpoint")
    };
    std::assert_matches!(&items[..], [RolloutItem::GoalCheckpoint(_)]);
    let Some(RolloutCmd::Persist { ack }) = rx.recv().await else {
        panic!("expected materialization")
    };
    ack.send(()).unwrap();
    let Some(RolloutCmd::Confirm { ack }) = rx.recv().await else {
        panic!("expected commit confirmation")
    };
    assert!(!first.is_finished());
    ack.send(Err(IoError::other("simulated unconfirmed commit")))
        .unwrap();
    assert!(first.await.unwrap().is_err());

    let sess = Arc::clone(&session);
    let restored = tokio::spawn(async move {
        crate::goal::request(
            &sess,
            "driver".into(),
            "stdio:test".into(),
            1,
            json!({"version":VERSION,"operation":"restore"}),
        )
        .await
    });
    // Reconfirm the queued item; do not append a duplicate.
    let Some(RolloutCmd::Persist { ack }) = rx.recv().await else {
        panic!("expected reconciliation")
    };
    ack.send(()).unwrap();
    let Some(RolloutCmd::Confirm { ack }) = rx.recv().await else {
        panic!("expected confirmation")
    };
    assert!(!restored.is_finished());
    ack.send(Ok(())).unwrap();
    assert_eq!(restored.await.unwrap().unwrap()["goal"], json!(goal));
    assert!(rx.try_recv().is_err());
}
