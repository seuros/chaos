//! Public-API tests for `chaos-amphetamine` — keeping the machine awake
//! while a turn does its work.
//!
//! The platform backends themselves (IOKit, systemd-inhibit, the no-op)
//! are covered elsewhere. What matters here is the state machine the
//! wrapper presents: enabled/disabled, idempotency, and the fact that
//! toggling never panics regardless of whether a real inhibitor exists
//! on the host. One dense pass covers every branch.

use chaos_amphetamine::SleepInhibitor;

#[test]
fn sleep_inhibitor_state_machine_survives_every_toggle_pattern() {
    // Enabled path: single on/off roundtrip exposes the turn state.
    let mut inhibitor = SleepInhibitor::new(true);
    inhibitor.set_turn_running(true);
    assert!(inhibitor.is_turn_running());
    inhibitor.set_turn_running(false);
    assert!(!inhibitor.is_turn_running());

    // Idempotent re-entry: three consecutive `true` calls must not leak
    // handles or panic when the backend is already active.
    inhibitor.set_turn_running(true);
    inhibitor.set_turn_running(true);
    inhibitor.set_turn_running(true);
    assert!(inhibitor.is_turn_running());
    inhibitor.set_turn_running(false);

    // Rapid oscillation stresses acquire/release ordering.
    inhibitor.set_turn_running(true);
    inhibitor.set_turn_running(false);
    inhibitor.set_turn_running(true);
    inhibitor.set_turn_running(false);

    // Disabled path: the wrapper still tracks the requested state, but
    // must never touch a platform backend.
    let mut disabled = SleepInhibitor::new(false);
    disabled.set_turn_running(true);
    assert!(disabled.is_turn_running());
    disabled.set_turn_running(false);
    assert!(!disabled.is_turn_running());
}
