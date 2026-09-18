use super::*;

fn task(status: TaskStatus) -> McpTask {
    McpTask {
        task_id: "owned".into(),
        status,
        status_message: None,
        created_at: "now".into(),
        last_updated_at: "now".into(),
        ttl: None,
        poll_interval: Some(1),
    }
}

#[tokio::test]
async fn terminal_observation_does_not_poll_again() {
    let mut events = observe_task(
        task(TaskStatus::Completed),
        CancellationToken::new(),
        || async {
            panic!("terminal tasks must not be polled");
            #[allow(unreachable_code)]
            Ok(task(TaskStatus::Working))
        },
    );
    assert!(matches!(
        events.recv().await,
        Some(TaskObservation::Status(_))
    ));
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn observation_failure_is_not_a_task_outcome_and_cancel_stops_polling() {
    let cancel = CancellationToken::new();
    let mut events = observe_task(task(TaskStatus::Working), cancel.clone(), || async {
        anyhow::bail!("disconnected")
    });
    assert!(matches!(
        events.recv().await,
        Some(TaskObservation::Status(_))
    ));
    assert!(matches!(
        events.recv().await,
        Some(TaskObservation::Unavailable(_))
    ));
    cancel.cancel();
    assert!(events.recv().await.is_none());
}

#[test]
fn polling_hints_are_bounded() {
    assert_eq!(
        poll_delay(&task(TaskStatus::Working)),
        Duration::from_millis(250)
    );
    let mut task = task(TaskStatus::Working);
    task.poll_interval = Some(u64::MAX);
    assert_eq!(poll_delay(&task), Duration::from_secs(60));
}
