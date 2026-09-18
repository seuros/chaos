use super::*;

#[test]
fn empty_completion_preserves_failure_but_next_turn_resets() {
    for failure in [
        AgentStatus::Errored("provider failed".into()),
        AgentStatus::Interrupted,
    ] {
        assert_eq!(
            preserve_turn_failure(&failure, AgentStatus::Completed(None)),
            failure
        );
        assert_eq!(
            preserve_turn_failure(&failure, AgentStatus::Running),
            AgentStatus::Running
        );
    }
    assert_eq!(
        preserve_turn_failure(&AgentStatus::Running, AgentStatus::Completed(None)),
        AgentStatus::Completed(None),
    );
}
