use super::*;

#[test]
fn persisted_states_reconstruct_and_only_allow_declared_transitions() {
    let cases = [
        (ReviewAttemptState::Selection, ReviewAttemptState::Spawn),
        (
            ReviewAttemptState::Spawn,
            ReviewAttemptState::ModelExecution,
        ),
        (
            ReviewAttemptState::ModelExecution,
            ReviewAttemptState::OutputParse,
        ),
        (
            ReviewAttemptState::OutputParse,
            ReviewAttemptState::SubmissionUnknown,
        ),
        (
            ReviewAttemptState::SubmissionUnknown,
            ReviewAttemptState::Acknowledged,
        ),
    ];
    for (from, to) in cases {
        assert!(ReviewAttemptWorkflow::from_state(from).permits(to));
    }

    assert!(
        !ReviewAttemptWorkflow::from_state(ReviewAttemptState::Acknowledged)
            .permits(ReviewAttemptState::SubmissionUnknown)
    );
    assert!(
        ReviewAttemptWorkflow::from_state(ReviewAttemptState::SubmissionUnknown)
            .permits(ReviewAttemptState::TerminalFailure)
    );
    for state in [
        ReviewAttemptState::Selection,
        ReviewAttemptState::Spawn,
        ReviewAttemptState::ModelExecution,
        ReviewAttemptState::OutputParse,
    ] {
        assert!(ReviewAttemptWorkflow::from_state(state).permits(ReviewAttemptState::Cancelled));
    }
    assert!(
        !ReviewAttemptWorkflow::from_state(ReviewAttemptState::SubmissionUnknown)
            .permits(ReviewAttemptState::Cancelled)
    );
}
