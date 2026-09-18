use super::*;

#[test]
fn workflow_transitions_retry_and_persisted_status_replay() {
    let mut wf = MinionJobItemWorkflow::new();
    assert_eq!(wf.current_state(), MinionJobItemLifecycleState::Pending);

    assert!(wf.start());
    assert_eq!(wf.current_state(), MinionJobItemLifecycleState::Running);

    assert!(wf.complete());
    assert_eq!(wf.current_state(), MinionJobItemLifecycleState::Completed);

    let mut wf = MinionJobItemWorkflow::new();
    wf.start();
    assert!(wf.retry());
    assert_eq!(wf.current_state(), MinionJobItemLifecycleState::Pending);

    assert!(wf.start());
    assert_eq!(wf.current_state(), MinionJobItemLifecycleState::Running);

    let mut wf = MinionJobItemWorkflow::new();
    assert!(!wf.retry());
    assert_eq!(wf.current_state(), MinionJobItemLifecycleState::Pending);

    let cases = [
        (
            MinionJobItemStatus::Pending,
            MinionJobItemLifecycleState::Pending,
        ),
        (
            MinionJobItemStatus::Running,
            MinionJobItemLifecycleState::Running,
        ),
        (
            MinionJobItemStatus::Completed,
            MinionJobItemLifecycleState::Completed,
        ),
        (
            MinionJobItemStatus::Failed,
            MinionJobItemLifecycleState::Failed,
        ),
    ];
    for (status, expected) in cases {
        let wf = MinionJobItemWorkflow::from_status(status);
        assert_eq!(wf.current_state(), expected);
    }
}
