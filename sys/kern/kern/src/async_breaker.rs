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

#[expect(
    clippy::expect_used,
    reason = "every breaker policy is built from compile-time constants"
)]
pub(crate) fn breaker_policy(
    failure_threshold: u32,
    failure_window: Duration,
    half_open_timeout: Duration,
    success_threshold: u32,
) -> Policy {
    Threshold::new(NonZeroU32::new(failure_threshold), None, 0)
        .zip(NonZeroU32::new(success_threshold))
        .and_then(|(threshold, successes)| {
            let recovery = Recovery::new(half_open_timeout, 0.0, successes, NonZeroU32::MIN)?;
            Policy::new(threshold, failure_window, recovery, None)
        })
        .expect("breaker policy constants are valid")
}

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
    pub(crate) fn new(
        name: impl Into<Arc<str>>,
        failure_threshold: u32,
        failure_window: Duration,
        half_open_timeout: Duration,
        success_threshold: u32,
    ) -> Self {
        let policy = breaker_policy(
            failure_threshold,
            failure_window,
            half_open_timeout,
            success_threshold,
        );
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
