//! Per-client authentication circuit breaker.
//!
//! Reuses `breaker-machines` to fail fast when the active provider needs
//! credentials and none are present. Without this guard every turn re-runs the
//! async auth resolution (including a token refresh round-trip) and builds a
//! request that is doomed to a 401. Once the breaker trips we reject the turn
//! immediately until the half-open window elapses, at which point a single
//! probe re-checks the live auth state — so connecting an account is picked up
//! on the user's next turn (and a login can force it closed via [`reset`]).
//!
//! The breaker is owned by a single [`crate::client::ModelClientState`], so it
//! shares the lifetime and credential realm of exactly one provider/auth pair.
//! That avoids the cross-session contamination a process-wide registry keyed by
//! provider id alone would invite (e.g. a logged-out session opening the
//! breaker for a different session that authenticates the same provider id with
//! different credentials).
//!
//! Mirrors the manual `opened_at` bookkeeping used by the MCP breaker: the
//! `record_*` API doesn't drive Open→HalfOpen on its own, so we track the open
//! timestamp ourselves and `reset()` once the timeout passes.

use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;

use breaker_machines::CircuitBreaker;

/// Backoff window before a probe is allowed through. Kept short so a fresh
/// login unlocks the next turn rather than stalling behind a long timeout.
const AUTH_HALF_OPEN_TIMEOUT: Duration = Duration::from_secs(2);

/// Outcome of consulting the breaker before resolving credentials.
pub(crate) enum AuthGate {
    /// Open and within backoff — reject without touching auth or the network.
    RejectFastFail,
    /// Resolve auth as normal, then report the result via [`AuthBreaker::record`].
    Probe,
}

struct AuthBreakerState {
    breaker: CircuitBreaker,
    opened_at: Option<Instant>,
}

/// A single provider/credential realm's auth breaker.
pub(crate) struct AuthBreaker {
    state: Mutex<AuthBreakerState>,
}

impl std::fmt::Debug for AuthBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let open = self
            .state
            .lock()
            .map(|guard| guard.breaker.is_open())
            .unwrap_or(false);
        f.debug_struct("AuthBreaker").field("open", &open).finish()
    }
}

impl AuthBreaker {
    pub(crate) fn new(provider_id: &str) -> Self {
        let name = format!("auth:{provider_id}");
        Self {
            state: Mutex::new(AuthBreakerState {
                // A single missing-credentials probe is enough to open: there
                // is nothing to retry until the user acts.
                breaker: CircuitBreaker::builder(name)
                    .failure_threshold(1)
                    .failure_window_secs(60.0)
                    .half_open_timeout_secs(2.0)
                    .success_threshold(1)
                    .build(),
                opened_at: None,
            }),
        }
    }

    /// Decide whether to probe auth or fail fast.
    pub(crate) fn check(&self) -> AuthGate {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.breaker.is_open() {
            match guard.opened_at {
                Some(opened_at) if opened_at.elapsed() < AUTH_HALF_OPEN_TIMEOUT => {
                    return AuthGate::RejectFastFail;
                }
                _ => {
                    // Backoff elapsed (or open without a timestamp) — let one
                    // probe through to re-check whether the user has logged in.
                    guard.breaker.reset();
                    guard.opened_at = None;
                }
            }
        }
        AuthGate::Probe
    }

    /// Record whether credentials were present after a probe.
    pub(crate) fn record(&self, authenticated: bool) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if authenticated {
            if guard.breaker.is_open() {
                // Proof of valid credentials — close immediately instead of
                // making the user wait out the backoff window.
                guard.breaker.reset();
            } else {
                guard.breaker.record_success_and_maybe_close(0.0);
            }
            guard.opened_at = None;
        } else {
            let was_open = guard.breaker.is_open();
            guard.breaker.record_failure_and_maybe_trip(0.0);
            if !was_open && guard.breaker.is_open() {
                guard.opened_at = Some(Instant::now());
            }
        }
    }

    /// Force the breaker closed. Called when auth is reloaded (e.g. after the
    /// user connects an account) so the next turn probes the fresh state
    /// instead of waiting out the backoff window.
    pub(crate) fn reset(&self) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.breaker.reset();
        guard.opened_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_credentials_trip_then_close_on_auth() {
        let breaker = AuthBreaker::new("test-provider");

        // A fresh breaker always probes.
        assert!(matches!(breaker.check(), AuthGate::Probe));

        // A missing-credentials probe opens it; subsequent turns within the
        // backoff window reject without probing again.
        breaker.record(false);
        assert!(matches!(breaker.check(), AuthGate::RejectFastFail));

        // A confirmed-authenticated probe force-closes, restoring normal
        // probing without waiting for the wall-clock backoff.
        breaker.record(true);
        assert!(matches!(breaker.check(), AuthGate::Probe));
    }

    #[test]
    fn reset_reopens_probing_after_login() {
        let breaker = AuthBreaker::new("test-provider-reset");
        breaker.record(false);
        assert!(matches!(breaker.check(), AuthGate::RejectFastFail));
        breaker.reset();
        assert!(matches!(breaker.check(), AuthGate::Probe));
    }

    #[test]
    fn authenticated_provider_keeps_probing() {
        let breaker = AuthBreaker::new("test-provider-present");
        assert!(matches!(breaker.check(), AuthGate::Probe));
        breaker.record(true);
        assert!(matches!(breaker.check(), AuthGate::Probe));
    }
}
