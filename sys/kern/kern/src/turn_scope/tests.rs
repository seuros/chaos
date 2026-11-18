use std::time::Duration;

use pretty_assertions::assert_eq;

use super::TurnScope;
use crate::exec::ExecExpiration;

fn timeout_ms(expiration: &ExecExpiration) -> Option<u64> {
    match expiration {
        ExecExpiration::Timeout(duration) => Some(duration.as_millis() as u64),
        ExecExpiration::DefaultTimeout | ExecExpiration::Cancellation(_) => None,
    }
}

#[tokio::test(start_paused = true)]
async fn never_keeps_the_tool_timeouts() {
    let scope = TurnScope::never();
    assert_eq!(scope.remaining(), None);
    assert_eq!(
        timeout_ms(&scope.exec_expiration(Some(600_000))),
        Some(600_000)
    );
    assert!(matches!(
        scope.exec_expiration(None),
        ExecExpiration::DefaultTimeout
    ));
}

#[tokio::test(start_paused = true)]
async fn a_bounded_turn_clamps_longer_tool_timeouts() {
    let scope = TurnScope::after(Duration::from_secs(5));
    assert_eq!(
        timeout_ms(&scope.exec_expiration(Some(600_000))),
        Some(5_000)
    );
    assert_eq!(timeout_ms(&scope.exec_expiration(Some(1_000))), Some(1_000));
    assert_eq!(timeout_ms(&scope.exec_expiration(None)), Some(5_000));
}
