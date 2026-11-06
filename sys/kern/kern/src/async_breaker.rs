use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use breaker_machines::CircuitError;
use breaker_machines::MemoryStorage;
use breaker_machines::StorageBackend;
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
struct BreakerClock(Instant);

impl breaker_machines::time::Clock for BreakerClock {
    fn now_secs(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
}

/// In-memory breaker storage on the tokio clock, so paused-time tests drive
/// the cooldown.
pub(crate) fn tokio_clock_storage() -> Arc<dyn StorageBackend> {
    Arc::new(MemoryStorage::with_clock(Box::new(BreakerClock(
        Instant::now(),
    ))))
}

pub(crate) struct AsyncCircuitBreaker {
    breaker: breaker_machines::AsyncCircuitBreaker,
}

impl fmt::Debug for AsyncCircuitBreaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncCircuitBreaker")
            .field("state", &self.breaker.state_name())
            .field("retry_after", &self.retry_after())
            .finish()
    }
}

impl AsyncCircuitBreaker {
    pub(crate) fn new(
        name: impl Into<String>,
        failure_threshold: usize,
        failure_window: Duration,
        half_open_timeout: Duration,
        success_threshold: usize,
    ) -> Self {
        let breaker = breaker_machines::AsyncCircuitBreaker::builder(name)
            .failure_threshold(failure_threshold)
            .failure_window_secs(failure_window.as_secs_f64())
            .half_open_timeout_secs(half_open_timeout.as_secs_f64())
            .success_threshold(success_threshold)
            .storage(tokio_clock_storage())
            .build_async();
        Self { breaker }
    }

    /// Return the delay before the next half-open probe may run.
    ///
    /// Callers that own a background actor can sleep for this duration and
    /// invoke [`Self::call`] when it elapses instead of waiting for new traffic.
    pub(crate) fn retry_after(&self) -> Option<Duration> {
        self.breaker.retry_after().map(Duration::from_secs_f64)
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
            Err(
                CircuitError::Open { .. }
                | CircuitError::HalfOpenLimitReached { .. }
                | CircuitError::BulkheadFull { .. },
            ) => Err(BreakerError::Open),
            Err(CircuitError::Storage(error)) => {
                tracing::error!("circuit breaker storage failed: {error}");
                Err(BreakerError::Open)
            }
        }
    }
}

#[cfg(test)]
mod tests;
