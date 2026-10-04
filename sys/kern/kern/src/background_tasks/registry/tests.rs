use super::*;

#[tokio::test]
async fn ordinary_terminal_records_cannot_be_reopened_via_registration() {
    let registry = TaskRegistry::default();
    registry.begin_submission("call").await;
    let id = TaskRegistry::submission_id("call");
    registry
        .complete(&id, TaskState::Succeeded, None, None)
        .await;
    let mut task = registry.get(&id).await.unwrap();
    task.state = TaskState::Running;
    registry.register(task).await;
    assert_eq!(registry.get(&id).await.unwrap().state, TaskState::Succeeded);
}

#[tokio::test]
async fn only_undelivered_machine_recovery_success_can_be_withdrawn() {
    let registry = TaskRegistry::default();
    registry.begin_submission("recovery").await;
    let id = TaskRegistry::submission_id("recovery");
    let mut task = registry.get(&id).await.unwrap();
    task.source = Some(TaskSource::MachineRecovery);
    task.state = TaskState::Running;
    registry.register(task).await;
    registry
        .complete(&id, TaskState::Succeeded, None, None)
        .await;
    let mut task = registry.get(&id).await.unwrap();
    task.state = TaskState::Running;
    registry.register(task).await;
    assert_eq!(registry.get(&id).await.unwrap().state, TaskState::Running);
    registry
        .complete(&id, TaskState::Succeeded, None, None)
        .await;
    registry
        .acknowledge(std::slice::from_ref(&id), "wake")
        .await;
    let mut task = registry.get(&id).await.unwrap();
    task.state = TaskState::Running;
    registry.register(task).await;
    assert_eq!(registry.get(&id).await.unwrap().state, TaskState::Succeeded);
}

#[tokio::test]
async fn closed_wake_policy_cannot_restart_cancelled_observers() {
    let registry = TaskRegistry::default();
    registry.set_policy(WakePolicy::Closed).await;
    registry.set_policy(WakePolicy::Enabled).await;
    assert_eq!(
        registry.subscribe().borrow().wake_policy,
        WakePolicy::Closed
    );
    assert!(registry.observer_cancel.is_cancelled());
}

#[tokio::test]
async fn machine_recovery_pending_success_is_never_replayed() {
    let registry = TaskRegistry::default();
    registry
        .register(BackgroundTask {
            id: "machine-recovery:old".into(),
            source: Some(TaskSource::MachineRecovery),
            state: TaskState::Succeeded,
            status_message: None,
            created_at: "now".into(),
            updated_at: "now".into(),
            result: None,
            origin_call_id: None,
            origin_turn_id: None,
            execution_id: None,
            ready: true,
            notify: true,
            delivered: false,
        })
        .await;
    let restored = TaskRegistry::default();
    restored.restore(&registry.take_journal().await).await;
    assert!(restored.pending().await.is_empty());
    assert_eq!(restored.list().await[0].state, TaskState::Cancelled);
}

#[tokio::test]
async fn completion_is_gated_deduplicated_and_resume_stable() {
    let registry = TaskRegistry::default();
    registry
        .register(BackgroundTask {
            id: "task".into(),
            source: None,
            state: TaskState::Running,
            status_message: None,
            created_at: "now".into(),
            updated_at: "now".into(),
            result: None,
            origin_call_id: Some("call".into()),
            ready: false,
            notify: true,
            delivered: false,
            origin_turn_id: None,
            execution_id: None,
        })
        .await;
    registry
        .complete("task", TaskState::Succeeded, None, Some(Value::Null))
        .await;
    assert!(registry.pending().await.is_empty());
    registry.response_recorded("call").await;
    let mut late_registration = registry.get("task").await.unwrap();
    late_registration.ready = false;
    registry.register(late_registration).await;
    assert_eq!(registry.pending().await.len(), 1);
    registry.set_policy(WakePolicy::Interrupted).await;
    assert!(registry.pending().await.is_empty());
    registry.set_policy(WakePolicy::Enabled).await;
    let late_registration = registry.get("task").await.unwrap();
    registry.acknowledge(&["task".into()], "turn").await;
    registry.register(late_registration).await;
    registry
        .complete("task", TaskState::Failed, None, None)
        .await;
    assert!(registry.pending().await.is_empty());
    let restored = TaskRegistry::default();
    restored.restore(&registry.take_journal().await).await;
    assert!(restored.pending().await.is_empty());
    assert_eq!(
        restored.get("task").await.unwrap().state,
        TaskState::Succeeded
    );
}

#[tokio::test]
async fn explicit_result_preceding_exit_observation_is_acknowledged() {
    let registry = TaskRegistry::default();
    registry.begin_submission("exec").await;
    let id = TaskRegistry::submission_id("exec");
    registry.complete(&id, TaskState::Running, None, None).await;
    registry.response_recorded("exec").await;
    registry.result_read(id.clone(), "read").await;
    registry.response_recorded("read").await;
    registry
        .complete(&id, TaskState::Succeeded, None, Some(Value::Null))
        .await;
    assert!(registry.get(&id).await.unwrap().delivered);
    assert!(registry.pending().await.is_empty());
}

#[tokio::test]
async fn interrupted_continuation_is_not_replayed_on_recovery() {
    let registry = TaskRegistry::default();
    registry.continuation_started("turn").await;
    let restored = TaskRegistry::default();
    restored.restore(&registry.take_journal().await).await;
    assert_eq!(
        restored.subscribe().borrow().wake_policy,
        WakePolicy::Interrupted
    );
}

#[tokio::test]
async fn unknown_submission_is_not_reexecuted_or_notified_after_error_response() {
    let registry = TaskRegistry::default();
    registry.begin_submission("remote").await;
    registry.response_recorded("remote").await;
    assert_eq!(
        registry
            .get(&TaskRegistry::submission_id("remote"))
            .await
            .unwrap()
            .state,
        TaskState::SubmissionUnknown
    );
    assert!(registry.pending().await.is_empty());
}

#[test]
fn retained_results_have_a_utf8_safe_bound() {
    let result = bound_result(Value::String("💥".repeat(100_000)));
    assert_eq!(result["truncated"], true);
    assert!(serde_json::to_vec(&result).unwrap().len() <= 64 * 1024);
}
