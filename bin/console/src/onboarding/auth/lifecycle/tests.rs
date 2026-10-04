use super::*;

#[test]
fn login_completion_requires_a_waiting_flow_then_acknowledgement() {
    let mut state = SignIn::default();
    assert!(!state.transition(SignInState::ChatGptSuccessMessage));
    assert!(state.transition(SignInState::ChatGptDeviceCode(
        super::super::ContinueWithDeviceCodeState {
            device_code: None,
            cancel: None
        },
    )));
    assert!(state.transition(SignInState::ChatGptSuccessMessage));
    assert!(state.transition(SignInState::ChatGptSuccess));
    assert!(!state.transition(SignInState::ChatGptSuccessMessage));
}
