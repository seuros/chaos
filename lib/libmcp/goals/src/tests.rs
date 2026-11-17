use super::*;

#[test]
fn version_and_payload_are_strict() {
    assert!(Request::parse(json!({"version":2,"operation":"restore"})).is_ok());
    for invalid in [
        json!({"version":1,"operation":"restore"}),
        json!({"version":2,"operation":"complete"}),
        json!({"version":2,"operation":"restore","conversation":"another"}),
        json!({"version":2,"operation":"evaluate","revision":1,"claim":"done","evidence":[]}),
        json!({"version":2,"operation":"evaluate","revision":1,"claim":"x".repeat(8193)}),
    ] {
        assert!(Request::parse(invalid).is_err());
    }
}

#[test]
fn lifecycle_moves_only_along_approved_edges() {
    assert_eq!(
        Status::Active.next(GoalLifecycleEvent::Check(true)),
        Some(Status::Checking)
    );
    assert_eq!(
        Status::Checking.next(GoalLifecycleEvent::Pass(true)),
        Some(Status::Complete)
    );
    assert_eq!(
        Status::Checking.next(GoalLifecycleEvent::Retry(true)),
        Some(Status::Active)
    );
    assert_eq!(
        Status::Complete.next(GoalLifecycleEvent::Reopen(true)),
        Some(Status::Active)
    );
    assert_eq!(
        Status::Complete.next(GoalLifecycleEvent::Pause(true)),
        Some(Status::Paused)
    );
    assert_eq!(Status::Active.next(GoalLifecycleEvent::Check(false)), None);
    assert_eq!(Status::Active.next(GoalLifecycleEvent::Pass(true)), None);
    assert!(!Status::Paused.accepts(GoalLifecycleEvent::Reopen(true)));
}

#[test]
fn status_wire_names_are_stable() {
    assert_eq!(
        serde_json::to_value([
            Status::Active,
            Status::Checking,
            Status::Complete,
            Status::Paused
        ])
        .ok(),
        Some(json!(["active", "checking", "complete", "paused"]))
    );
}
