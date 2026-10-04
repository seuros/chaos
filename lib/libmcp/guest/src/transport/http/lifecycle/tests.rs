use super::*;

#[test]
fn recovery_retains_initialized_notification_state_and_close_is_terminal() {
    let mut lifecycle = Lifecycle::default();
    assert!(lifecycle.session(HttpSessionEvent::BeginInitialization));
    assert!(lifecycle.session(HttpSessionEvent::Negotiated));
    assert!(!lifecycle.initialized());
    assert!(lifecycle.session(HttpSessionEvent::Initialized));
    assert!(lifecycle.session(HttpSessionEvent::Recover));
    assert!(lifecycle.initialized());
    assert!(lifecycle.session(HttpSessionEvent::BeginInitialization));
    assert!(lifecycle.session(HttpSessionEvent::Negotiated));
    assert!(lifecycle.initialized());
    assert!(lifecycle.session(HttpSessionEvent::Restored));
    lifecycle.close();
    lifecycle.close();
    assert!(!lifecycle.session(HttpSessionEvent::BeginInitialization));
    assert!(!lifecycle.session(HttpSessionEvent::Initialized));
    assert!(!lifecycle.session(HttpSessionEvent::Recover));
    assert!(!lifecycle.sse_enabled());
    assert!(!lifecycle.sse(HttpSseEvent::Connect));
}

#[test]
fn sse_405_disable_survives_session_recovery() {
    let mut lifecycle = Lifecycle::default();
    assert!(lifecycle.sse(HttpSseEvent::Connect));
    assert!(lifecycle.sse(HttpSseEvent::Disable));
    assert!(lifecycle.sse(HttpSseEvent::Stop));
    assert!(!lifecycle.sse_enabled());
    assert!(!lifecycle.sse(HttpSseEvent::Connect));
    lifecycle.close();
    assert!(!lifecycle.sse_enabled());
}
