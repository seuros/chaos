use super::*;
use crate::tools::context::ToolPayload;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
async fn dropping_a_workflow_dispatch_waits_for_its_task_destructor() -> TestResult {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let cancellation = CancellationToken::new();
    let router = crate::chaos::built_tools(&session, &turn, &[], &cancellation).await?;
    let held = session.services.tool_scheduler.acquire(false).await?;
    let tasks = tokio_util::task::TaskTracker::new();
    let tracker = Arc::new(tokio::sync::Mutex::new(
        crate::turn_diff_tracker::TurnDiffTracker::new(),
    ));
    let runtime = ToolCallRuntime::new(router, session.clone(), turn, tracker);
    let mut dispatch = Box::pin(runtime.handle_tool_call_with_source(
        ToolCall {
            tool_name: "view_image".into(),
            tool_namespace: None,
            call_id: "drop-leaf".into(),
            payload: ToolPayload::Function {
                arguments: "{}".into(),
            },
        },
        ToolCallSource::Workflow {
            replay: false,
            tasks: tasks.clone(),
        },
        cancellation,
    ));
    assert!(futures::poll!(dispatch.as_mut()).is_pending());
    assert!(!tasks.is_empty());
    drop(dispatch);
    tasks.close();
    tokio::time::timeout(std::time::Duration::from_secs(2), tasks.wait()).await?;
    assert!(tasks.is_empty());
    drop(held);
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.services.tool_scheduler.acquire(false),
    )
    .await??;
    drop(next);
    Ok(())
}

#[tokio::test]
async fn workflow_rechecks_group_visibility_after_waiting_for_its_permit() -> TestResult {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [crate::tools::groups::FILESYSTEM],
        true,
    )?;
    let cancellation = CancellationToken::new();
    let router = crate::chaos::built_tools(&session, &turn, &[], &cancellation).await?;
    let tracker = Arc::new(tokio::sync::Mutex::new(
        crate::turn_diff_tracker::TurnDiffTracker::new(),
    ));
    let held = session.services.tool_scheduler.acquire(false).await?;
    let tasks = tokio_util::task::TaskTracker::new();
    let call = ToolCall {
        tool_name: "view_image".into(),
        tool_namespace: None,
        call_id: "blocked-leaf".into(),
        payload: ToolPayload::Function {
            arguments: r#"{"path":"/never-open-this-file"}"#.into(),
        },
    };
    let runtime = ToolCallRuntime::new(router, session.clone(), turn, tracker);
    let mut pending = Box::pin(runtime.handle_tool_call_with_source(
        call,
        ToolCallSource::Workflow {
            replay: false,
            tasks: tasks.clone(),
        },
        cancellation,
    ));
    assert!(futures::poll!(pending.as_mut()).is_pending());
    tokio::task::yield_now().await;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [crate::tools::groups::FILESYSTEM],
        false,
    )?;
    drop(held);
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), pending).await?;
    assert!(result.is_err());
    tasks.close();
    tokio::time::timeout(std::time::Duration::from_secs(2), tasks.wait()).await?;
    Ok(())
}

#[tokio::test]
async fn cancelling_queued_acquisition_does_not_block_later_calls() -> TestResult {
    let scheduler = ToolSchedulerActor::spawn();
    let held = scheduler.acquire(false).await?;
    let mut queued = Box::pin(scheduler.acquire(false));
    assert!(futures::poll!(queued.as_mut()).is_pending());
    tokio::task::yield_now().await;
    drop(queued);
    drop(held);
    let next =
        tokio::time::timeout(std::time::Duration::from_secs(2), scheduler.acquire(false)).await??;
    drop(next);
    Ok(())
}

#[tokio::test]
async fn cancellation_after_enqueue_before_acknowledgement_releases_granted_permit() -> TestResult {
    let scheduler = ToolSchedulerActor::spawn();
    let mut acquisition = Box::pin(scheduler.acquire(false));
    assert!(futures::poll!(acquisition.as_mut()).is_pending());
    // The actor may grant the permit, but the acquisition future has not yet
    // observed its mailbox acknowledgement. Its cleanup must already be armed.
    tokio::task::yield_now().await;
    drop(acquisition);
    let next =
        tokio::time::timeout(std::time::Duration::from_secs(2), scheduler.acquire(false)).await??;
    drop(next);
    Ok(())
}

#[tokio::test]
async fn scheduler_clones_share_parallel_and_exclusive_execution() -> TestResult {
    let scheduler = ToolSchedulerActor::spawn();
    let nested = scheduler.clone();
    let first = scheduler.acquire(true).await?;
    let second = nested.acquire(true).await?;
    let mut exclusive = Box::pin(nested.acquire(false));
    assert!(futures::poll!(exclusive.as_mut()).is_pending());
    drop(first);
    tokio::task::yield_now().await;
    assert!(futures::poll!(exclusive.as_mut()).is_pending());
    drop(second);
    let lease = tokio::time::timeout(std::time::Duration::from_secs(2), exclusive).await??;
    drop(lease);
    Ok(())
}

#[tokio::test]
async fn separate_sessions_do_not_share_execution_permits() -> TestResult {
    let first = ToolSchedulerActor::spawn();
    let second = ToolSchedulerActor::spawn();
    let held = first.acquire(false).await?;
    let independent =
        tokio::time::timeout(std::time::Duration::from_secs(2), second.acquire(false)).await??;
    drop((held, independent));
    Ok(())
}

#[tokio::test]
async fn independently_built_runtimes_use_the_session_scheduler() -> TestResult {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let router = Arc::new(ToolRouter::from_config(
        &turn.tools_config,
        crate::tools::router::ToolRouterParams {
            mcp_tools: None,
            app_tools: None,
            dynamic_tools: &[],
            catalog_tools: vec![],
            halluacinate: None,
            plan_mode: false,
            tool_groups: None,
        },
    ));
    let tracker = Arc::new(tokio::sync::Mutex::new(
        crate::turn_diff_tracker::TurnDiffTracker::new(),
    ));
    let first = ToolCallRuntime::new(
        router.clone(),
        session.clone(),
        turn.clone(),
        tracker.clone(),
    );
    let nested = ToolCallRuntime::new(router, session.clone(), turn, tracker);
    let held = first.scheduler.acquire(false).await?;
    let mut next = Box::pin(nested.scheduler.acquire(false));
    assert!(futures::poll!(next.as_mut()).is_pending());
    drop(held);
    let lease = tokio::time::timeout(std::time::Duration::from_secs(2), next).await??;
    drop(lease);
    Ok(())
}

#[test]
fn exec_command_abort_retains_execution_output_format() {
    let call = ToolCall {
        tool_name: "exec_command".to_string(),
        tool_namespace: None,
        call_id: "exec-abort".to_string(),
        payload: crate::tools::context::ToolPayload::Function {
            arguments: r#"{"cmd":"sleep 30"}"#.to_string(),
        },
    };

    assert_eq!(
        ToolCallRuntime::abort_message(&call, 1.25),
        "Wall time: 1.2 seconds\naborted by user"
    );
}

#[test]
fn exclusive_waits_for_active_parallel_calls() {
    let mut schedule = ToolSchedule::default();
    schedule
        .active
        .insert("running".to_string(), ToolExecutionMode::Parallel);
    let (exclusive_tx, mut exclusive_rx) = oneshot::channel();
    schedule.queue.push_back(QueuedToolCall {
        call_id: "exclusive".to_string(),
        supports_parallel: false,
        permit: exclusive_tx,
    });

    schedule.dispatch();

    assert_eq!(schedule.queue.len(), 1);
    assert!(exclusive_rx.try_recv().is_err());
    assert_eq!(schedule.active.len(), 1);
}

#[test]
fn leading_parallel_calls_are_released_together() {
    let mut schedule = ToolSchedule::default();
    let (first_tx, mut first_rx) = oneshot::channel();
    let (second_tx, mut second_rx) = oneshot::channel();
    let (exclusive_tx, mut exclusive_rx) = oneshot::channel();
    schedule.queue.push_back(QueuedToolCall {
        call_id: "first".to_string(),
        supports_parallel: true,
        permit: first_tx,
    });
    schedule.queue.push_back(QueuedToolCall {
        call_id: "second".to_string(),
        supports_parallel: true,
        permit: second_tx,
    });
    schedule.queue.push_back(QueuedToolCall {
        call_id: "exclusive".to_string(),
        supports_parallel: false,
        permit: exclusive_tx,
    });

    schedule.dispatch();

    assert_eq!(first_rx.try_recv(), Ok(()));
    assert_eq!(second_rx.try_recv(), Ok(()));
    assert!(exclusive_rx.try_recv().is_err());
    assert_eq!(schedule.active.len(), 2);
    assert_eq!(schedule.queue.len(), 1);
}

#[test]
fn releasing_a_call_dispatches_the_next_call() {
    let mut schedule = ToolSchedule::default();
    let (first_tx, mut first_rx) = oneshot::channel();
    let (second_tx, mut second_rx) = oneshot::channel();
    schedule.enqueue(QueuedToolCall {
        call_id: "first".to_string(),
        supports_parallel: false,
        permit: first_tx,
    });
    schedule.enqueue(QueuedToolCall {
        call_id: "second".to_string(),
        supports_parallel: false,
        permit: second_tx,
    });

    assert_eq!(first_rx.try_recv(), Ok(()));
    assert!(second_rx.try_recv().is_err());

    schedule.release("first");
    assert_eq!(second_rx.try_recv(), Ok(()));
    assert_eq!(
        schedule.active.get("second"),
        Some(&ToolExecutionMode::Exclusive)
    );
}
