use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;

use breaker_machines::CircuitError;
use breaker_machines::MemoryStorage;
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

pub(crate) struct AsyncCircuitBreaker {
    breaker: breaker_machines::AsyncCircuitBreaker,
    retry_at: Arc<Mutex<Option<Instant>>>,
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
        let retry_at = Arc::new(Mutex::new(None));
        let opened = retry_at.clone();
        let closed = retry_at.clone();
        let probing = retry_at.clone();
        let breaker = breaker_machines::AsyncCircuitBreaker::builder(name)
            .failure_threshold(failure_threshold)
            .failure_window_secs(failure_window.as_secs_f64())
            .half_open_timeout_secs(half_open_timeout.as_secs_f64())
            .success_threshold(success_threshold)
            .storage(Arc::new(MemoryStorage::with_clock(Box::new(BreakerClock(
                Instant::now(),
            )))))
            .on_open(move |_| {
                *opened.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(Instant::now() + half_open_timeout);
            })
            .on_close(move |_| {
                *closed.lock().unwrap_or_else(PoisonError::into_inner) = None;
            })
            .on_half_open(move |_| {
                *probing.lock().unwrap_or_else(PoisonError::into_inner) = None;
            })
            .build_async();
        Self { breaker, retry_at }
    }

    /// Return the delay before the next half-open probe may run.
    ///
    /// Callers that own a background actor can sleep for this duration and
    /// invoke [`Self::call`] when it elapses instead of waiting for new traffic.
    pub(crate) fn retry_after(&self) -> Option<Duration> {
        self.retry_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
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
