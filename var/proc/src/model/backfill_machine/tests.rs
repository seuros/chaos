use super::*;

#[test]
fn backfill_workflow_transitions_and_persisted_status_replay() {
    let mut wf = BackfillWorkflow::new();
    assert_eq!(wf.current_state(), BackfillLifecycleState::Pending);

    assert!(wf.start());
    assert_eq!(wf.current_state(), BackfillLifecycleState::Running);

    assert!(wf.complete());
    assert_eq!(wf.current_state(), BackfillLifecycleState::Complete);

    let mut wf = BackfillWorkflow::new();
    assert!(!wf.complete());
    assert_eq!(wf.current_state(), BackfillLifecycleState::Pending);

    let mut wf = BackfillWorkflow::new();
    wf.start();
    assert!(!wf.start());
    assert_eq!(wf.current_state(), BackfillLifecycleState::Running);

    for (status, expected) in [
        (BackfillStatus::Pending, BackfillLifecycleState::Pending),
        (BackfillStatus::Running, BackfillLifecycleState::Running),
        (BackfillStatus::Complete, BackfillLifecycleState::Complete),
    ] {
        let wf = BackfillWorkflow::from_status(status);
        assert_eq!(wf.current_state(), expected);
    }
}
