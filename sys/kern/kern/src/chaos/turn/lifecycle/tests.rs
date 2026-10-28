use super::*;
use TurnControlEvent::*;
use TurnControlState as State;

fn apply_sequence(
    lifecycle: &mut TurnLifecycle,
    events: impl IntoIterator<Item = TurnControlEvent>,
) {
    for event in events {
        lifecycle.apply(event);
    }
}

#[test]
fn tool_follow_up_requires_settlement_and_preparation() {
    let mut lifecycle = TurnLifecycle::default();
    lifecycle.apply(Sample);
    assert!(lifecycle.machine.handle(Sample).is_err());
    assert!(lifecycle.machine.handle(Finish).is_err());
    lifecycle.apply(StreamEnded);
    assert!(lifecycle.machine.handle(FollowUp).is_err());
    apply_sequence(
        &mut lifecycle,
        [
            AttemptSettled,
            FollowUp,
            Sample,
            StreamEnded,
            AttemptSettled,
            Finish,
            Complete,
        ],
    );
    assert_eq!(lifecycle.machine.current_state(), State::Completed);
}

#[test]
fn compaction_and_continuations_reenter_preparation() {
    let mut lifecycle = TurnLifecycle::default();
    apply_sequence(
        &mut lifecycle,
        [
            Compact,
            Compacted,
            Sample,
            StreamEnded,
            AttemptSettled,
            Compact,
            CompactFailed,
            Compact,
            Compacted,
            Sample,
            StreamEnded,
            AttemptSettled,
            Finish,
            HookContinuation,
            Sample,
            StreamEnded,
            AttemptSettled,
            Finish,
            GoalContinuation,
            Sample,
            StreamEnded,
            AttemptSettled,
            Finish,
            Complete,
        ],
    );
    assert_eq!(lifecycle.machine.current_state(), State::Completed);
}

#[test]
fn failed_attempt_can_back_off_or_repair_context_but_not_complete() {
    let mut lifecycle = TurnLifecycle::default();
    apply_sequence(&mut lifecycle, [Sample, AttemptSettled, Retry(0)]);
    assert!(lifecycle.machine.handle(Sample).is_err());
    assert!(lifecycle.machine.handle(Complete).is_err());
    apply_sequence(
        &mut lifecycle,
        [RetryElapsed, Sample, AttemptSettled, RepairContext, Sample],
    );
    assert_eq!(lifecycle.machine.current_state(), State::Sampling);
    lifecycle.close(false);
    assert_eq!(lifecycle.machine.current_state(), State::Failed);
}

#[test]
fn cancellation_blocks_work_until_cleanup_finishes() {
    for events in [
        vec![],
        vec![Sample],
        vec![Sample, StreamEnded],
        vec![Sample, AttemptSettled],
        vec![Sample, AttemptSettled, Retry(0)],
        vec![Compact],
        vec![Sample, StreamEnded, AttemptSettled, Finish],
    ] {
        let mut lifecycle = TurnLifecycle::default();
        apply_sequence(&mut lifecycle, events);
        lifecycle.cancel();
        lifecycle.cancel();
        assert_eq!(lifecycle.machine.current_state(), State::Cancelling);
        assert!(!lifecycle.machine.is_finished());
        for event in [Sample, RetryElapsed, AttemptSettled, Complete] {
            assert!(lifecycle.machine.handle(event).is_err());
        }
        lifecycle.close(false);
        assert_eq!(lifecycle.machine.current_state(), State::Cancelled);
    }
}

#[test]
fn early_returns_are_not_successful_completion() {
    let mut failed = TurnLifecycle::default();
    failed.close(false);
    assert_eq!(failed.machine.current_state(), State::Failed);

    let mut parked = TurnLifecycle::default();
    parked.apply(Park);
    parked.close(false);
    assert_eq!(parked.machine.current_state(), State::Parked);

    let mut cancelled = TurnLifecycle::default();
    cancelled.close(true);
    assert_eq!(cancelled.machine.current_state(), State::Cancelled);
}

#[test]
fn terminal_states_reject_late_events() {
    for events in [
        vec![Sample, StreamEnded, AttemptSettled, Finish, Complete],
        vec![Stop],
        vec![Park],
        vec![Fail],
        vec![Cancel, CleanupCompleted],
    ] {
        let mut lifecycle = TurnLifecycle::default();
        apply_sequence(&mut lifecycle, events);
        let terminal = lifecycle.machine.current_state();
        assert!(lifecycle.machine.is_finished());
        for event in [
            Sample,
            StreamEnded,
            AttemptSettled,
            FollowUp,
            RepairContext,
            Retry(0),
            RetryElapsed,
            Compact,
            Compacted,
            CompactFailed,
            Finish,
            HookContinuation,
            GoalContinuation,
            Complete,
            Stop,
            Park,
            Fail,
            Cancel,
            CleanupCompleted,
        ] {
            assert!(lifecycle.machine.handle(event).is_err());
            assert_eq!(lifecycle.machine.current_state(), terminal);
        }
        lifecycle.close(true);
        assert_eq!(lifecycle.machine.current_state(), terminal);
    }
}

#[test]
fn dispatched_tools_prevent_replaying_a_failed_request() {
    let mut lifecycle = TurnLifecycle::default();
    apply_sequence(&mut lifecycle, [Sample, StreamEnded, AttemptSettled]);
    for calls in [1, 2, usize::MAX] {
        assert!(!lifecycle.try_retry(calls));
        assert_eq!(lifecycle.machine.current_state(), State::Boundary);
    }
    lifecycle.close(false);
    assert_eq!(lifecycle.machine.current_state(), State::Failed);
}
