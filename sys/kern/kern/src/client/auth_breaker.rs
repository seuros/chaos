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

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use breaker_machines::AsyncCircuitBreaker;
use breaker_machines::CircuitError;
use breaker_machines::PredicateClassifier;

use crate::async_breaker::tokio_clock_storage;
use crate::error::ChaosErr;

/// Backoff window before a probe is allowed through. Kept short so a fresh
/// login unlocks the next turn rather than stalling behind a long timeout.
const AUTH_HALF_OPEN_TIMEOUT: Duration = Duration::from_secs(2);

/// A single provider/credential realm's auth breaker.
pub(crate) struct AuthBreaker {
    breaker: AsyncCircuitBreaker,
}

impl std::fmt::Debug for AuthBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthBreaker")
            .field("open", &self.breaker.is_open())
            .finish()
    }
}

impl AuthBreaker {
    pub(crate) fn new(provider_id: &str) -> Self {
        // A single missing-credentials probe is enough to open: there is
        // nothing to retry until the user acts.
        let breaker = AsyncCircuitBreaker::builder(format!("auth:{provider_id}"))
            .failure_threshold(1)
            .failure_window_secs(60.0)
            .half_open_timeout_secs(AUTH_HALF_OPEN_TIMEOUT.as_secs_f64())
            .success_threshold(1)
            .storage(tokio_clock_storage())
            .failure_classifier(Arc::new(PredicateClassifier::new(|ctx| {
                ctx.error
                    .downcast_ref::<ChaosErr>()
                    .is_some_and(|err| matches!(err, ChaosErr::ProviderAuthMissing(_)))
            })))
            .build_async();
        Self { breaker }
    }

    /// Run the auth probe unless the breaker is open within its backoff
    /// window, in which case `rejected()` is returned without probing. Only a
    /// `ProviderAuthMissing` outcome trips the breaker; any other error passes
    /// through untouched.
    pub(crate) async fn probe<T, F, Fut>(
        &self,
        op: F,
        rejected: impl FnOnce() -> ChaosErr,
    ) -> Result<T, ChaosErr>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ChaosErr>>,
    {
        match self.breaker.call(op).await {
            Ok(value) => Ok(value),
            Err(CircuitError::Execution(err)) => Err(err),
            Err(_) => Err(rejected()),
        }
    }

    /// Force the breaker closed. Called when auth is reloaded (e.g. after the
    /// user connects an account) so the next turn probes the fresh state
    /// instead of waiting out the backoff window.
    pub(crate) fn reset(&self) {
        self.breaker.reset();
    }
}

#[cfg(test)]
mod tests;
