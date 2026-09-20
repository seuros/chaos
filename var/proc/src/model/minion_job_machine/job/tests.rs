use super::*;

#[test]
fn workflow_transitions_and_persisted_status_restore() {
    let mut wf = MinionJobWorkflow::new();
    assert_eq!(wf.current_state(), MinionJobLifecycleState::Pending);

    assert!(wf.start());
    assert_eq!(wf.current_state(), MinionJobLifecycleState::Running);

    assert!(wf.complete());
    assert_eq!(wf.current_state(), MinionJobLifecycleState::Completed);

    let mut wf = MinionJobWorkflow::new();
    wf.start();
    assert!(wf.fail());
    assert_eq!(wf.current_state(), MinionJobLifecycleState::Failed);

    let mut wf = MinionJobWorkflow::new();
    assert!(wf.cancel());
    assert_eq!(wf.current_state(), MinionJobLifecycleState::Cancelled);

    let mut wf = MinionJobWorkflow::new();
    wf.start();
    assert!(wf.cancel());
    assert_eq!(wf.current_state(), MinionJobLifecycleState::Cancelled);

    let mut wf = MinionJobWorkflow::new();
    assert!(!wf.complete());
    assert_eq!(wf.current_state(), MinionJobLifecycleState::Pending);

    let cases = [
        (MinionJobStatus::Pending, MinionJobLifecycleState::Pending),
        (MinionJobStatus::Running, MinionJobLifecycleState::Running),
        (
            MinionJobStatus::Completed,
            MinionJobLifecycleState::Completed,
        ),
        (MinionJobStatus::Failed, MinionJobLifecycleState::Failed),
        (
            MinionJobStatus::Cancelled,
            MinionJobLifecycleState::Cancelled,
        ),
    ];
    for (status, expected) in cases {
        let wf = MinionJobWorkflow::from_status(status);
        assert_eq!(wf.current_state(), expected);
    }
}
