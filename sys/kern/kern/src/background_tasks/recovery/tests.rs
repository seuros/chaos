use super::*;
use chaos_ipc::background_tasks::{BackgroundTask, TaskJournalEvent, WakePolicy};

#[tokio::test]
async fn recovery_marks_local_execution_lost_without_starting_work() {
    let (session, _) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let record = BackgroundTask {
        id: "exec-generation".into(),
        source: Some(TaskSource::Exec { session_id: 9 }),
        state: TaskState::Running,
        status_message: None,
        created_at: "now".into(),
        updated_at: "now".into(),
        result: None,
        origin_call_id: Some("call".into()),
        origin_turn_id: Some("turn".into()),
        execution_id: None,
        ready: true,
        notify: true,
        delivered: false,
    };
    session
        .recover_background_tasks(&[
            RolloutItem::BackgroundTask(TaskJournalEvent::Upsert {
                task: Box::new(record),
            }),
            RolloutItem::BackgroundTask(TaskJournalEvent::WakePolicy {
                policy: WakePolicy::Interrupted,
            }),
        ])
        .await;
    let tasks = session.services.internal_task_store.list().await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, TaskState::Lost);
    assert!(session.active_turn.lock().await.is_none());
    assert_eq!(
        session
            .services
            .internal_task_store
            .subscribe()
            .borrow()
            .wake_policy,
        WakePolicy::Interrupted
    );
}
