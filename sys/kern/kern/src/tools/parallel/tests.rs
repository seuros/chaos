use super::*;

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
