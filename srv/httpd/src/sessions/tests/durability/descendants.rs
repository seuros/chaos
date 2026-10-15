use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn root_completion_waits_for_descendant_quiet_without_a_root_activity_notification() {
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    let id = Uuid::new_v4().to_string();
    fixture.admit(&id).await.unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Execute(turn, _) if turn == id
    ));
    supervisor.tree_quiet.store(false, Ordering::SeqCst);
    supervisor.events.send(complete(&id)).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_until(&fixture.session, |data| {
            data.active
                .as_ref()
                .is_some_and(|active| active.completion_outcome.is_some())
        }),
    )
    .await
    .unwrap();
    assert_eq!(fixture.capacity.available_permits(), 0);
    assert_eq!(
        fixture.persisted_turn(&id).await.state,
        ExecutionTurnState::Running
    );
    assert_ne!(
        fixture.session.turn(&id).unwrap().status,
        TurnStatus::Completed
    );
    // Child exit need not change the root activity watch. The supervisor must
    // independently re-confirm the tree, not release from root TurnComplete.
    supervisor.tree_quiet.store(true, Ordering::SeqCst);
    status(&fixture.session, SessionStatus::Idle).await;
    assert_eq!(fixture.capacity.available_permits(), 1);
    assert_eq!(
        fixture.persisted_turn(&id).await.state,
        ExecutionTurnState::Completed
    );
    fixture.session.close().unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    supervisor.task.await.unwrap();
    fixture.db.close().await;
}
