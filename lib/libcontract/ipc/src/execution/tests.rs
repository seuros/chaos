use super::*;
use pretty_assertions::assert_eq;

#[test]
fn every_state_round_trips_through_its_persisted_name() {
    for state in SessionLifecycleState::ALL {
        let json = serde_json::to_string(state).unwrap();
        assert_eq!(json, format!("\"{}\"", state.as_str()));
        assert_eq!(
            serde_json::from_str::<SessionLifecycleState>(&json).unwrap(),
            *state
        );
    }
    for state in TurnLifecycleState::ALL {
        let json = serde_json::to_string(state).unwrap();
        assert_eq!(json, format!("\"{}\"", state.as_str()));
        assert_eq!(
            serde_json::from_str::<TurnLifecycleState>(&json).unwrap(),
            *state
        );
    }
    for state in InvocationLifecycleState::ALL {
        let json = serde_json::to_string(state).unwrap();
        assert_eq!(json, format!("\"{}\"", state.as_str()));
        assert_eq!(
            serde_json::from_str::<InvocationLifecycleState>(&json).unwrap(),
            *state
        );
    }
}

#[test]
fn persisted_names_are_frozen() {
    let names = |states: &[&'static str]| states.join(",");
    assert_eq!(
        names(
            &SessionLifecycleState::ALL
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
        ),
        "starting,idle,running,closing,unknown,closed,failed"
    );
    assert_eq!(
        names(
            &TurnLifecycleState::ALL
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
        ),
        concat!(
            "persisting,accepted,running,waiting_background,interrupting,",
            "unknown,completed,failed,interrupted,timed_out"
        )
    );
    assert_eq!(
        names(
            &InvocationLifecycleState::ALL
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
        ),
        "started,completed,unknown"
    );
    assert_eq!(
        names(
            &SessionLifecycleState::LEDGER
                .iter()
                .map(|s| s.ledger_str().unwrap())
                .collect::<Vec<_>>()
        ),
        "idle,active,unknown,closed"
    );
}

#[test]
fn ledger_session_names_round_trip_and_refuse_daemon_only_states() {
    #[derive(Serialize, Deserialize)]
    struct Row {
        #[serde(with = "ledger")]
        state: SessionLifecycleState,
    }
    for state in SessionLifecycleState::LEDGER {
        let json = serde_json::to_string(&Row { state: *state }).unwrap();
        let row: Row = serde_json::from_str(&json).unwrap();
        assert_eq!(row.state, *state);
        assert_eq!(
            SessionLifecycleState::from_ledger(state.ledger_str().unwrap()),
            Some(*state)
        );
    }
    for state in [
        SessionLifecycleState::Starting,
        SessionLifecycleState::Closing,
        SessionLifecycleState::Failed,
    ] {
        assert!(serde_json::to_string(&Row { state }).is_err());
    }
    assert!(serde_json::from_str::<Row>(r#"{"state":"running"}"#).is_err());
    assert_eq!(SessionLifecycleState::from_ledger("starting"), None);
}

#[test]
fn closed_and_failed_sessions_refuse_every_event() {
    for state in [SessionLifecycleState::Closed, SessionLifecycleState::Failed] {
        assert!(state.is_final());
        for event in [
            SessionLifecycleEvent::Ready,
            SessionLifecycleEvent::Busy,
            SessionLifecycleEvent::Settle,
            SessionLifecycleEvent::Reconcile,
            SessionLifecycleEvent::Lose,
            SessionLifecycleEvent::Close,
            SessionLifecycleEvent::Shut,
            SessionLifecycleEvent::Resolve,
            SessionLifecycleEvent::Fail,
        ] {
            let mut current = state;
            assert!(current.fire(event).is_err());
            assert_eq!(current, state);
        }
    }
}

#[test]
fn only_an_idle_session_becomes_busy() {
    for state in SessionLifecycleState::ALL {
        let mut current = *state;
        let accepted = current.fire(SessionLifecycleEvent::Busy).is_ok();
        assert_eq!(accepted, *state == SessionLifecycleState::Idle, "{state}");
        let expected = if accepted {
            SessionLifecycleState::Running
        } else {
            *state
        };
        assert_eq!(current, expected);
    }
}

#[test]
fn unfinished_sessions_cannot_shut() {
    for state in [
        SessionLifecycleState::Starting,
        SessionLifecycleState::Running,
        SessionLifecycleState::Unknown,
    ] {
        let mut current = state;
        assert!(current.fire(SessionLifecycleEvent::Shut).is_err());
        assert_eq!(current, state);
    }
}

#[test]
fn undispatched_turns_cannot_complete() {
    for state in [TurnLifecycleState::Persisting, TurnLifecycleState::Accepted] {
        for outcome in [TurnLifecycleState::Completed, TurnLifecycleState::Failed] {
            let mut current = state;
            assert!(current.conclude(outcome).is_err());
            assert_eq!(current, state);
        }
    }
    let mut accepted = TurnLifecycleState::Accepted;
    accepted.conclude(TurnLifecycleState::Interrupted).unwrap();
    assert_eq!(accepted, TurnLifecycleState::Interrupted);
}

#[test]
fn terminal_turns_are_immutable() {
    for state in TurnLifecycleState::ALL
        .iter()
        .filter(|state| state.is_final())
    {
        for outcome in TurnLifecycleState::ALL {
            let mut current = *state;
            assert!(current.conclude(*outcome).is_err());
            assert_eq!(current, *state);
        }
    }
    assert!(!TurnLifecycleState::Unknown.is_final());
}

#[test]
fn unknown_turns_resolve_to_any_outcome() {
    for outcome in [
        TurnLifecycleState::Completed,
        TurnLifecycleState::Failed,
        TurnLifecycleState::Interrupted,
        TurnLifecycleState::TimedOut,
        TurnLifecycleState::Unknown,
    ] {
        let mut current = TurnLifecycleState::Unknown;
        current.conclude(outcome).unwrap();
        assert_eq!(current, outcome);
    }
    let mut current = TurnLifecycleState::Unknown;
    assert!(current.conclude(TurnLifecycleState::Running).is_err());
}

#[test]
fn invocations_finish_once() {
    let mut started = InvocationLifecycleState::Started;
    started.fire(InvocationLifecycleEvent::Complete).unwrap();
    assert_eq!(started, InvocationLifecycleState::Completed);
    for state in [
        InvocationLifecycleState::Completed,
        InvocationLifecycleState::Unknown,
    ] {
        let mut current = state;
        assert!(current.fire(InvocationLifecycleEvent::Complete).is_err());
        assert!(current.fire(InvocationLifecycleEvent::Lose).is_err());
        assert_eq!(current, state);
    }
}
