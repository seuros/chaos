use super::*;

#[test]
fn operations_and_completed_prompts_reject_cancel() {
    for prompt in [false, true] {
        let mut turn = TurnState::new(prompt);
        assert!(turn.is_live());
        if !prompt {
            assert!(!turn.request_cancel());
        }
        assert!(!turn.complete());
        assert!(!turn.is_live());
        assert!(!turn.request_cancel());
        assert!(!turn.complete());
        turn.cancel_finished();
        turn.abandon();
        assert!(!turn.is_live());
    }
}

#[test]
fn native_cancel_must_join_even_when_prompt_completes_first() {
    let mut turn = TurnState::new(true);
    assert!(turn.request_cancel());
    assert!(!turn.request_cancel());
    assert!(turn.is_live());
    assert!(turn.complete());
    assert!(turn.complete());
    assert!(!turn.is_live());
    assert!(!turn.request_cancel());
    turn.cancel_finished();
    turn.cancel_finished();
    assert!(!turn.complete());
    assert!(!turn.is_live());
}

#[test]
fn native_cancel_acknowledgement_does_not_complete_the_prompt() {
    let mut turn = TurnState::new(true);
    assert!(turn.request_cancel());
    turn.cancel_finished();
    assert!(turn.is_live());
    assert!(!turn.request_cancel());
    assert!(!turn.complete());
    assert!(!turn.is_live());
}

#[test]
fn abandoning_an_inflight_turn_is_terminal_despite_late_callbacks() {
    let mut turn = TurnState::new(true);
    assert!(turn.request_cancel());
    assert!(turn.complete());
    turn.abandon();
    turn.cancel_finished();
    assert!(!turn.is_live());
    assert!(!turn.request_cancel());
    assert!(!turn.complete());
}
