use super::*;

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
    assert_eq!(registry.pending().await.len(), 1);
    registry.set_policy(WakePolicy::Interrupted).await;
    assert!(registry.pending().await.is_empty());
    registry.set_policy(WakePolicy::Enabled).await;
    registry.acknowledge(&["task".into()], "turn").await;
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
