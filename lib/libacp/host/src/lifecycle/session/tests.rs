use super::*;

#[test]
fn opening_and_replay_remain_exclusive_until_the_guard_releases() {
    let setup = Turn::new_operation(false);
    let prompt = Turn::new_operation(true);
    let mut session = Session::opening(setup.clone());
    assert!(session.check(&setup).is_ok());
    assert!(session.begin(prompt.clone()).is_err());
    assert!(session.request_cancel().is_none());
    session.release(&setup);
    assert!(session.check(&setup).is_err());
    session.begin(prompt.clone()).unwrap();
    // A delayed old guard must never clear a newer turn.
    session.release(&setup);
    assert!(session.check(&prompt).is_ok());
    assert!(session.begin(setup).is_err());
    assert!(session.request_cancel().is_some());
    assert!(session.request_cancel().is_none());
}

#[test]
fn invalidation_retains_cleanup_identity_but_never_readmits_work() {
    for opening in [false, true] {
        let setup = Turn::new_operation(false);
        let turn = Turn::new_operation(true);
        let mut session = Session::opening(setup.clone());
        let active = if opening {
            setup
        } else {
            session.release(&setup);
            session.begin(turn.clone()).unwrap();
            turn
        };
        session.invalidate();
        session.invalidate();
        assert!(session.is_invalid());
        assert!(Arc::ptr_eq(session.active().unwrap(), &active));
        assert!(session.check(&active).is_err());
        session.release(&active);
        assert!(session.active().is_none());
        assert!(session.is_invalid());
        session.invalidate();
        assert!(session.begin(Turn::new_operation(true)).is_err());
    }
}

#[test]
fn an_idle_session_can_be_invalidated_without_an_active_turn() {
    let turn = Turn::new_operation(false);
    let mut session = Session::opening(turn.clone());
    session.release(&turn);
    session.invalidate();
    assert!(session.is_invalid());
    assert!(session.active().is_none());
    assert!(session.begin(Turn::new_operation(true)).is_err());
}

#[test]
fn controls_coexist_with_prompts_but_not_opening_or_other_controls() {
    let setup = Turn::new_operation(false);
    let prompt = Turn::new_operation(true);
    let control = Turn::new_operation(false);
    let mut session = Session::opening(setup.clone());
    assert!(session.begin_control(control.clone()).is_err());
    session.release(&setup);
    session.begin(prompt.clone()).unwrap();
    session.begin_control(control.clone()).unwrap();
    assert!(session.check(&prompt).is_ok());
    assert!(session.check(&control).is_ok());
    assert!(session.begin_control(Turn::new_operation(false)).is_err());
    assert!(Arc::ptr_eq(&session.request_cancel().unwrap(), &prompt));
    assert!(prompt.cancel.is_cancelled());
    assert!(!control.cancel.is_cancelled());
    session.release(&control);
    assert!(session.check(&control).is_err());
    assert!(session.check(&prompt).is_ok());
    assert!(session.begin(Turn::new_operation(true)).is_err());
}

#[test]
fn prompt_completion_does_not_expire_a_concurrent_control() {
    let setup = Turn::new_operation(false);
    let prompt = Turn::new_operation(true);
    let control = Turn::new_operation(false);
    let mut session = Session::opening(setup.clone());
    session.release(&setup);
    session.begin(prompt.clone()).unwrap();
    session.begin_control(control.clone()).unwrap();
    session.release(&prompt);
    assert!(session.check(&control).is_ok());
    let next = Turn::new_operation(true);
    session.begin(next.clone()).unwrap();
    session.release(&prompt);
    session.release(&control);
    assert!(session.check(&next).is_ok());
}

#[test]
fn invalidation_rejects_both_prompt_and_control_contexts() {
    let setup = Turn::new_operation(false);
    let prompt = Turn::new_operation(true);
    let control = Turn::new_operation(false);
    let mut session = Session::opening(setup.clone());
    session.release(&setup);
    session.begin(prompt.clone()).unwrap();
    session.begin_control(control.clone()).unwrap();
    session.invalidate();
    assert!(session.check(&prompt).is_err());
    assert!(session.check(&control).is_err());
    session.release(&control);
    session.release(&prompt);
    assert!(session.is_invalid());
    assert!(session.begin_control(Turn::new_operation(false)).is_err());
}
