use std::error::Error;

use chaos_synopsis::runs::{
    MAX_ACTIVE_RUNS, MAX_SNAPSHOT_BYTES, MAX_TERMINAL_RUNS, RunError, RunKind, RunOutcome,
    RunRegistry, RunStatus, StepOutcome, StepStatus, TERMINAL_RETENTION,
};
use serde_json::json;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn cloned_recorders_share_attempts_without_owning_the_executor() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    let id = lease.id();
    let recorder = lease.recorder();
    let observer = recorder.clone();
    recorder.start_step("ask", 1)?;
    observer.waiting_for_input("ask", 1)?;
    assert_eq!(
        registry.status(id)?.steps[0].status,
        StepStatus::WaitingForInput
    );
    drop(lease);
    let snapshot = registry.status(id)?;
    assert_eq!(
        snapshot.status,
        RunStatus::Terminal(RunOutcome::Interrupted)
    );
    assert_eq!(
        snapshot.steps[0].status,
        StepStatus::Terminal(StepOutcome::Cancelled)
    );
    std::assert_matches!(
        observer.start_step("late", 1),
        Err(RunError::AlreadyTerminal)
    );
    std::assert_matches!(
        recorder.finish_step("ask", 1, StepOutcome::Success, None),
        Err(RunError::AlreadyTerminal)
    );
    Ok(())
}

#[test]
fn recorders_neither_keep_sessions_alive_nor_rewrite_completed_runs() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    let recorder = lease.recorder();
    recorder.start_step("call", 1)?;
    recorder.finish_step("call", 1, StepOutcome::Success, None)?;
    lease.finish(RunOutcome::Success, None)?;
    std::assert_matches!(
        recorder.start_step("next", 1),
        Err(RunError::AlreadyTerminal)
    );
    drop(registry);
    std::assert_matches!(recorder.start_step("next", 1), Err(RunError::SessionClosed));
    Ok(())
}

#[test]
fn workflow_and_synopsis_share_the_active_budget() -> TestResult {
    let registry = RunRegistry::default();
    let mut leases = Vec::new();
    for index in 0..MAX_ACTIVE_RUNS {
        let kind = if index % 2 == 0 {
            RunKind::Workflow
        } else {
            RunKind::Synopsis
        };
        leases.push(registry.start(kind)?);
    }
    std::assert_matches!(
        registry.start(RunKind::Workflow).err(),
        Some(RunError::ActiveLimit)
    );
    let Some(finished) = leases.pop() else {
        return Err("missing test lease".into());
    };
    finished.finish(RunOutcome::Success, None)?;
    let next = registry.start(RunKind::Workflow)?;
    assert_eq!(registry.status(next.id())?.status, RunStatus::Running);
    Ok(())
}

#[test]
fn run_id_is_not_authority_across_sessions_or_restarts() -> TestResult {
    let owner = RunRegistry::default();
    let other_session = RunRegistry::default();
    let lease = owner.start(RunKind::Workflow)?;
    let id = lease.id();
    assert!(owner.status(id).is_ok());
    std::assert_matches!(
        other_session.status(id),
        Err(RunError::UnknownRun(unknown)) if unknown == id
    );
    std::assert_matches!(
        other_session.cancel(id),
        Err(RunError::UnknownRun(unknown)) if unknown == id
    );
    let simulated_restart = RunRegistry::default();
    std::assert_matches!(simulated_restart.status(id), Err(RunError::UnknownRun(_)));
    assert!(!lease.cancellation().is_cancelled());
    Ok(())
}

#[test]
fn cancellation_waits_for_executor_cleanup_and_prevents_new_attempts() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    let id = lease.id();
    let token = lease.cancellation();
    lease.start_step("ask", 1)?;
    lease.waiting_for_input("ask", 1)?;

    assert_eq!(registry.cancel(id)?.status, RunStatus::Cancelling);
    assert!(token.is_cancelled());
    std::assert_matches!(lease.start_step("next", 1), Err(RunError::Cancelling));
    assert_eq!(
        registry.status(id)?.steps[0].status,
        StepStatus::WaitingForInput
    );
    lease.finish_step("ask", 1, StepOutcome::Cancelled, None)?;
    let final_state = lease.finish(RunOutcome::Cancelled, None)?;
    assert_eq!(
        final_state.status,
        RunStatus::Terminal(RunOutcome::Cancelled)
    );
    assert_eq!(registry.cancel(id)?.status, final_state.status);
    Ok(())
}

#[test]
fn dropping_executor_lease_marks_interrupted_and_cancels_active_steps() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Synopsis)?;
    let id = lease.id();
    let token = lease.cancellation();
    lease.start_step("child", 1)?;
    drop(lease);

    assert!(token.is_cancelled());
    let snapshot = registry.status(id)?;
    assert_eq!(
        snapshot.status,
        RunStatus::Terminal(RunOutcome::Interrupted)
    );
    assert_eq!(
        snapshot.steps[0].status,
        StepStatus::Terminal(StepOutcome::Cancelled)
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn dropping_a_run_future_releases_its_registry_lease() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    let id = lease.id();
    let token = lease.cancellation();
    let mut future = Box::pin(async move {
        lease.start_step("pending", 1)?;
        token.cancelled().await;
        lease.finish_step("pending", 1, StepOutcome::Cancelled, None)?;
        lease.finish(RunOutcome::Cancelled, None)
    });
    assert!(futures::poll!(future.as_mut()).is_pending());
    drop(future);
    assert_eq!(
        registry.status(id)?.status,
        RunStatus::Terminal(RunOutcome::Interrupted)
    );
    Ok(())
}

#[test]
fn last_session_registry_drop_cancels_even_while_executor_lease_is_alive() -> TestResult {
    let registry = RunRegistry::default();
    let clone = registry.clone();
    let lease = registry.start(RunKind::Workflow)?;
    let token = lease.cancellation();
    drop(registry);
    assert!(!token.is_cancelled());
    drop(clone);
    assert!(token.is_cancelled());
    std::assert_matches!(lease.start_step("late", 1), Err(RunError::SessionClosed));
    Ok(())
}

#[test]
fn explicit_session_shutdown_cancels_work_without_waiting_for_cloned_handles() -> TestResult {
    let registry = RunRegistry::default();
    let executor_handle = registry.clone();
    let lease = executor_handle.start(RunKind::Synopsis)?;
    let id = lease.id();
    lease.start_step("child", 1)?;
    let token = lease.cancellation();

    registry.shutdown();

    assert!(token.is_cancelled());
    assert_eq!(executor_handle.status(id)?.status, RunStatus::Cancelling);
    std::assert_matches!(
        executor_handle.start(RunKind::Workflow).err(),
        Some(RunError::SessionClosed)
    );
    std::assert_matches!(lease.start_step("late", 1), Err(RunError::Cancelling));
    std::assert_matches!(
        lease.waiting_for_input("child", 1),
        Err(RunError::Cancelling)
    );
    lease.finish_step("child", 1, StepOutcome::Cancelled, None)?;
    assert_eq!(
        lease.finish(RunOutcome::Cancelled, None)?.status,
        RunStatus::Terminal(RunOutcome::Cancelled)
    );
    registry.collect_expired();
    Ok(())
}

#[test]
fn step_attempts_are_ordered_single_use_and_terminal_records_are_immutable() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    std::assert_matches!(
        lease.start_step("leaf", 2),
        Err(RunError::InvalidTransition)
    );
    lease.start_step("leaf", 1)?;
    std::assert_matches!(
        lease.start_step("leaf", 1),
        Err(RunError::InvalidTransition)
    );
    std::assert_matches!(
        lease.start_step("leaf", 2),
        Err(RunError::InvalidTransition)
    );
    lease.finish_step(
        "leaf",
        1,
        StepOutcome::Failure,
        Some(json!({"code":"missing_binding"})),
    )?;
    std::assert_matches!(
        lease.waiting_for_input("leaf", 1),
        Err(RunError::InvalidTransition)
    );
    std::assert_matches!(
        lease.finish_step("leaf", 1, StepOutcome::Success, None),
        Err(RunError::InvalidTransition)
    );
    lease.start_step("leaf", 2)?;
    lease.finish_step("leaf", 2, StepOutcome::Success, Some(json!({"value":7})))?;
    let snapshot = lease.finish(RunOutcome::Success, Some(json!({"ok":true})))?;
    assert_eq!(snapshot.steps.len(), 2);
    assert_eq!(
        snapshot.steps[0].detail,
        Some(json!({"code":"missing_binding"}))
    );
    assert_eq!(snapshot.steps[1].attempt, 2);
    Ok(())
}

#[test]
fn normal_completion_requires_all_step_attempts_to_finish() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    let id = lease.id();
    lease.start_step("still-running", 1)?;
    std::assert_matches!(
        lease.finish(RunOutcome::Success, None),
        Err(RunError::ActiveSteps)
    );
    // The rejected consuming finish also drops its armed lease: cancellation
    // and an explicit interrupted outcome, never false success.
    assert_eq!(
        registry.status(id)?.status,
        RunStatus::Terminal(RunOutcome::Interrupted)
    );
    Ok(())
}

#[test]
fn attempt_budget_is_enforced_at_runtime_in_addition_to_tree_preflight() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    for attempt in 1..=256 {
        lease.start_step("leaf", attempt)?;
        lease.finish_step("leaf", attempt, StepOutcome::Failure, None)?;
    }
    std::assert_matches!(lease.start_step("leaf", 257), Err(RunError::AttemptLimit));
    std::assert_matches!(lease.start_step("another", 1), Err(RunError::AttemptLimit));
    assert_eq!(lease.finish(RunOutcome::Failure, None)?.steps.len(), 256);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn terminal_snapshots_expire_at_one_hour_without_expiring_active_runs() -> TestResult {
    let registry = RunRegistry::default();
    let active = registry.start(RunKind::Synopsis)?;
    let finished = registry
        .start(RunKind::Workflow)?
        .finish(RunOutcome::Success, None)?;
    tokio::time::advance(TERMINAL_RETENTION - std::time::Duration::from_millis(1)).await;
    assert!(registry.status(finished.run_id).is_ok());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    std::assert_matches!(
        registry.status(finished.run_id),
        Err(RunError::UnknownRun(_))
    );
    assert_eq!(registry.status(active.id())?.status, RunStatus::Running);
    Ok(())
}

#[test]
fn terminal_retention_is_capped_and_cancellation_cannot_reopen_results() -> TestResult {
    let registry = RunRegistry::default();
    let first = registry
        .start(RunKind::Workflow)?
        .finish(RunOutcome::Success, None)?;
    for _ in 0..MAX_TERMINAL_RUNS {
        registry
            .start(RunKind::Synopsis)?
            .finish(RunOutcome::Failure, None)?;
    }
    std::assert_matches!(registry.status(first.run_id), Err(RunError::UnknownRun(_)));
    let last = registry
        .start(RunKind::Workflow)?
        .finish(RunOutcome::Success, Some(json!({"done":true})))?;
    let after_cancel = registry.cancel(last.run_id)?;
    assert_eq!(after_cancel.status, last.status);
    assert_eq!(after_cancel.result, last.result);
    Ok(())
}

#[test]
fn serialized_snapshots_bound_payload_and_worst_case_escaped_metadata() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    for index in 0..256 {
        // Worst-case escaping expands each control byte to six JSON bytes.
        let suffix = format!("{index:03}");
        let id = format!("{}{suffix}", "\u{0001}".repeat(125));
        lease.start_step(&id, 1)?;
        let detail = if index == 0 {
            Some(json!({"data":"a".repeat(700 * 1024)}))
        } else if index == 1 {
            Some(json!({"data":"b".repeat(700 * 1024)}))
        } else {
            None
        };
        lease.finish_step(&id, 1, StepOutcome::Success, detail)?;
    }
    let snapshot = lease.finish(
        RunOutcome::Success,
        Some(json!({"data":"c".repeat(MAX_SNAPSHOT_BYTES)})),
    )?;
    assert!(snapshot.truncated);
    assert!(!snapshot.steps[0].truncated);
    assert!(snapshot.steps[1].truncated);
    assert!(snapshot.steps[1].detail.is_none());
    assert!(snapshot.result.is_none());
    assert!(serde_json::to_vec(&snapshot)?.len() <= MAX_SNAPSHOT_BYTES);
    Ok(())
}

#[test]
fn malformed_step_updates_do_not_consume_payload_budget() -> TestResult {
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    std::assert_matches!(
        lease.finish_step(
            "unknown",
            1,
            StepOutcome::Failure,
            Some(json!({"huge":"a".repeat(MAX_SNAPSHOT_BYTES)}))
        ),
        Err(RunError::UnknownStep)
    );
    std::assert_matches!(lease.start_step("", 1), Err(RunError::AttemptLimit));
    std::assert_matches!(
        lease.start_step(&"x".repeat(129), 1),
        Err(RunError::AttemptLimit)
    );
    std::assert_matches!(lease.start_step("leaf", 0), Err(RunError::AttemptLimit));
    lease.start_step("leaf", 1)?;
    lease.finish_step("leaf", 1, StepOutcome::Success, Some(json!({"small":true})))?;
    let snapshot = lease.finish(RunOutcome::Success, None)?;
    assert!(!snapshot.truncated);
    Ok(())
}
