use std::fmt;
use std::future::Future;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use breaker_machines::CircuitError;
use breaker_machines::Policy;
use breaker_machines::Recovery;
use breaker_machines::Threshold;
use tokio::time::Instant;

/// Error returned by an async operation guarded by [`AsyncCircuitBreaker`].
#[derive(Debug)]
pub(crate) enum BreakerError<E> {
    Open,
    Operation(E),
}

impl<E: fmt::Display> fmt::Display for BreakerError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => f.write_str("circuit breaker is open"),
            Self::Operation(err) => err.fmt(f),
        }
    }
}

#[derive(Debug)]
pub(crate) struct BreakerClock(Instant);

impl breaker_machines::Clock for BreakerClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// The tokio clock, so paused-time tests drive the cooldown.
pub(crate) fn tokio_clock() -> BreakerClock {
    BreakerClock(Instant::now())
}

pub(crate) const fn breaker_policy(
    failure_threshold: u32,
    failure_window: Duration,
    half_open_timeout: Duration,
    success_threshold: u32,
) -> Policy {
    let Some(threshold) = Threshold::new(NonZeroU32::new(failure_threshold), None, 0) else {
        panic!("a breaker opens after at least one failure");
    };
    let Some(successes) = NonZeroU32::new(success_threshold) else {
        panic!("a breaker closes after at least one success");
    };
    let Some(recovery) = Recovery::new(half_open_timeout, 0.0, successes, NonZeroU32::MIN) else {
        panic!("an unjittered cooldown is a valid recovery");
    };
    match Policy::new(threshold, failure_window, recovery, None) {
        Some(policy) => policy,
        None => panic!("a breaker window is not zero"),
    }
}

pub(crate) const TREE_BRIDGE_BREAKER: Policy = breaker_policy(
    chaos_synopsis::tree::MAX_LEAF_ATTEMPTS as u32 + 1,
    Duration::from_secs(60),
    Duration::from_secs(30),
    2,
);

pub(crate) struct AsyncCircuitBreaker {
    breaker: breaker_machines::AsyncCircuitBreaker,
}

impl fmt::Debug for AsyncCircuitBreaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncCircuitBreaker")
            .field("state", &self.breaker.state())
            .field("retry_after", &self.retry_after())
            .finish()
    }
}

impl AsyncCircuitBreaker {
    pub(crate) fn new(name: impl Into<Arc<str>>, policy: Policy) -> Self {
        let breaker = breaker_machines::AsyncCircuitBreaker::builder(name, policy)
            .clock(tokio_clock())
            .build_async();
        Self { breaker }
    }

    /// Return the delay before the next half-open probe may run.
    ///
    /// Callers that own a background actor can sleep for this duration and
    /// invoke [`Self::call`] when it elapses instead of waiting for new traffic.
    pub(crate) fn retry_after(&self) -> Option<Duration> {
        self.breaker.retry_after()
    }

    pub(crate) async fn call<T, E, F, Fut>(&self, op: F) -> Result<T, BreakerError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: 'static,
    {
        match self.breaker.call(op).await {
            Ok(value) => Ok(value),
            Err(CircuitError::Execution(error)) => Err(BreakerError::Operation(error)),
            Err(CircuitError::Refused { .. }) => Err(BreakerError::Open),
            Err(CircuitError::Storage(error)) => {
                tracing::error!("circuit breaker storage failed: {error}");
                Err(BreakerError::Open)
            }
        }
    }
}

#[cfg(test)]
mod tests;
