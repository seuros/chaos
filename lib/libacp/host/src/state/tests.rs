use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::HostError;
use crate::protocol::schema::v1::{PromptRequest, PromptResponse};

#[derive(Default)]
struct Native {
    shutdowns: AtomicUsize,
    started: CancellationToken,
    release: CancellationToken,
}

impl AgentSession for Native {
    fn prompt(
        &self,
        _request: PromptRequest,
        _context: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<PromptResponse>> {
        Box::pin(async { unreachable!("lifecycle tests do not execute prompts") })
    }

    fn cancel(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn shutdown(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            self.started.cancel();
            self.release.cancelled().await;
            Ok(())
        })
    }
}

fn fixture() -> (Arc<State>, Arc<Slot>, Arc<Native>) {
    let state = State::new(ConnectionLimits::default());
    state.initialize(ClientCapabilities::default()).unwrap();
    let native = Arc::new(Native::default());
    let setup = Turn::new_operation(false);
    let slot = Slot::opening(
        native.clone(),
        setup.clone(),
        state.session_permits.clone().try_acquire_owned().unwrap(),
    );
    state
        .sessions
        .lock()
        .unwrap()
        .insert(SessionId::new("test"), slot.clone());
    drop(TurnGuard {
        slot: slot.clone(),
        turn: setup,
    });
    (state, slot, native)
}

fn prompt(slot: &Arc<Slot>) -> TurnGuard {
    let turn = Turn::new_operation(true);
    slot.lock().begin(turn.clone()).unwrap();
    TurnGuard {
        slot: slot.clone(),
        turn,
    }
}

#[test]
fn dropped_prompts_and_unjoined_cancellation_fail_closed() {
    for cancelling in [false, true] {
        let (_state, slot, _native) = fixture();
        let guard = prompt(&slot);
        let turn = guard.turn.clone();
        if cancelling {
            assert!(slot.lock().request_cancel().is_some());
            assert!(guard.complete());
        }
        drop(guard);
        assert!(slot.is_invalid());
        assert!(turn.cancel.is_cancelled());
        assert!(!turn.is_live());
        turn.cancel_finished();
        assert!(slot.lock().begin(Turn::new_operation(true)).is_err());
    }
}

#[test]
fn confirmed_completion_releases_only_after_native_cancellation_joins() {
    let (_state, slot, _native) = fixture();
    let guard = prompt(&slot);
    assert!(slot.lock().request_cancel().is_some());
    assert!(guard.complete());
    assert!(slot.lock().check(&guard.turn).is_err());
    assert!(slot.lock().begin(Turn::new_operation(true)).is_err());
    guard.turn.cancel_finished();
    assert!(!guard.complete());
    drop(guard);
    assert!(!slot.is_invalid());
    let next = prompt(&slot);
    assert!(!next.complete());
    drop(next);
    assert!(!slot.is_invalid());
}

#[test]
fn racing_cancellations_claim_exactly_one_native_hook() {
    let (_state, slot, _native) = fixture();
    let guard = prompt(&slot);
    let claims = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                if slot.lock().request_cancel().is_some() {
                    claims.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(claims.load(Ordering::SeqCst), 1);
    assert!(guard.turn.cancel.is_cancelled());
    guard.turn.cancel_finished();
    assert!(!guard.complete());
    drop(guard);
}

#[tokio::test]
async fn concurrent_cleanup_callers_join_the_same_confirmed_result() {
    let (state, slot, native) = fixture();
    let guard = prompt(&slot);
    let owner_state = state.clone();
    let owner = tokio::spawn(async move { owner_state.cleanup().await });
    native.started.cancelled().await;
    assert!(guard.turn.cancel.is_cancelled());
    assert!(slot.is_invalid());
    assert!(state.ready().is_err());
    assert!(state.initialize(ClientCapabilities::default()).is_err());
    let waiter_state = state.clone();
    let waiter = tokio::spawn(async move { waiter_state.cleanup().await });
    tokio::task::yield_now().await;
    assert!(!owner.is_finished());
    assert!(!waiter.is_finished());
    native.release.cancel();
    owner.await.unwrap().unwrap();
    waiter.await.unwrap().unwrap();
    state.cleanup().await.unwrap();
    assert_eq!(native.shutdowns.load(Ordering::SeqCst), 1);
    drop(guard);
    assert!(slot.is_invalid());
}

#[tokio::test]
async fn dropped_cleanup_owner_wakes_joiners_with_an_unconfirmed_result() {
    let (state, _slot, native) = fixture();
    let owner_state = state.clone();
    let owner = tokio::spawn(async move { owner_state.cleanup().await });
    native.started.cancelled().await;
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    assert!(matches!(
        state.cleanup().await,
        Err(HostError::ShutdownUnconfirmed)
    ));
    assert!(state.ready().is_err());
    assert!(state.initialize(ClientCapabilities::default()).is_err());
    assert_eq!(native.shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn cleanup_timeout_remains_unconfirmed_on_repeated_calls() {
    let (state, _slot, native) = fixture();
    assert!(matches!(
        state.cleanup().await,
        Err(HostError::ShutdownUnconfirmed)
    ));
    native.release.cancel(); // A later acknowledgement cannot revive this connection.
    assert!(matches!(
        state.cleanup().await,
        Err(HostError::ShutdownUnconfirmed)
    ));
    assert_eq!(native.shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn first_connection_failure_survives_cleanup_and_later_failures() {
    let (state, _slot, native) = fixture();
    state.fail("first");
    state.fail("second");
    assert_eq!(state.failure_reason(), Some("first"));
    assert!(state.ready().is_err());
    native.release.cancel();
    state.cleanup().await.unwrap();
    state.close();
    assert_eq!(state.failure_reason(), Some("first"));
    assert!(state.initialize(ClientCapabilities::default()).is_err());
}
