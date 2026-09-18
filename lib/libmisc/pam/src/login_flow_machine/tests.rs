use super::*;

#[test]
fn workflow_covers_browser_device_fallback_and_cancel_paths() {
    let mut wf = LoginFlowWorkflow::new();
    assert_eq!(wf.current_state(), LoginFlowLifecycleState::Idle);

    wf.start_browser();
    assert_eq!(wf.current_state(), LoginFlowLifecycleState::StartingBrowser);

    wf.browser_ready();
    assert_eq!(
        wf.current_state(),
        LoginFlowLifecycleState::WaitingForBrowser
    );

    wf.succeed();
    assert_eq!(wf.current_state(), LoginFlowLifecycleState::Succeeded);

    let mut wf = LoginFlowWorkflow::new();
    wf.start_device_code();
    assert_eq!(
        wf.current_state(),
        LoginFlowLifecycleState::RequestingDeviceCode
    );

    wf.device_code_unsupported();
    assert_eq!(wf.current_state(), LoginFlowLifecycleState::StartingBrowser);

    wf.browser_ready();
    assert_eq!(
        wf.current_state(),
        LoginFlowLifecycleState::WaitingForBrowser
    );

    let mut wf = LoginFlowWorkflow::new();
    wf.start_device_code();
    wf.device_code_ready();
    assert_eq!(
        wf.current_state(),
        LoginFlowLifecycleState::WaitingForDeviceCode
    );

    wf.cancel();
    assert_eq!(wf.current_state(), LoginFlowLifecycleState::Cancelled);
}

#[test]
fn illegal_transition_is_rejected_and_state_unchanged() {
    // Drive the raw machine so the wrapper's debug_assert does not fire:
    // succeeding from Idle is not a declared transition.
    let mut machine = DynamicLoginFlowLifecycle::new(());
    assert_eq!(machine.current_state(), LoginFlowLifecycleState::Idle);

    assert!(machine.handle(LoginFlowLifecycleEvent::Succeed).is_err());
    assert_eq!(machine.current_state(), LoginFlowLifecycleState::Idle);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "illegal LoginFlow transition: succeed")]
fn wrapper_panics_when_transition_drifts() {
    // The runner must never emit a Succeeded update without a legal
    // transition; the wrapper guards that invariant in debug builds.
    let mut wf = LoginFlowWorkflow::new();
    wf.succeed();
}
