use std::fmt;
use std::future::Future;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;

use breaker_machines::CircuitBreaker;

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

struct BreakerState {
    breaker: CircuitBreaker,
    opened_at: Option<Instant>,
}

/// Async-friendly adapter around `breaker-machines`.
///
/// `breaker-machines` only drives half-open transitions through its synchronous
/// `call` API. Async users therefore track the open timestamp and reset after
/// the configured timeout to admit a probe without holding a mutex across an
/// await.
pub(crate) struct AsyncCircuitBreaker {
    half_open_timeout: Duration,
    state: Mutex<BreakerState>,
}

impl fmt::Debug for AsyncCircuitBreaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let open = self
            .state
            .lock()
            .map(|state| state.breaker.is_open())
            .unwrap_or(false);
        f.debug_struct("AsyncCircuitBreaker")
            .field("open", &open)
            .field("half_open_timeout", &self.half_open_timeout)
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
        Self {
            half_open_timeout,
            state: Mutex::new(BreakerState {
                breaker: CircuitBreaker::builder(name.into())
                    .failure_threshold(failure_threshold)
                    .failure_window_secs(failure_window.as_secs_f64())
                    .half_open_timeout_secs(half_open_timeout.as_secs_f64())
                    .success_threshold(success_threshold)
                    .build(),
                opened_at: None,
            }),
        }
    }

    /// Return the delay before the next half-open probe may run.
    ///
    /// Callers that own a background actor can sleep for this duration and
    /// invoke [`Self::call`] when it elapses instead of waiting for new traffic.
    pub(crate) fn retry_after(&self) -> Option<Duration> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.breaker.is_open() {
            return None;
        }

        Some(
            state
                .opened_at
                .map(|opened_at| self.half_open_timeout.saturating_sub(opened_at.elapsed()))
                .unwrap_or(Duration::ZERO),
        )
    }

    pub(crate) async fn call<T, E, F, Fut>(&self, op: F) -> Result<T, BreakerError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.breaker.is_open() {
                if state
                    .opened_at
                    .is_some_and(|opened_at| opened_at.elapsed() < self.half_open_timeout)
                {
                    return Err(BreakerError::Open);
                }
                state.breaker.reset();
                state.opened_at = None;
            }
        }

        let started_at = Instant::now();
        let result = op().await;
        let duration = started_at.elapsed().as_secs_f64();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match result {
            Ok(value) => {
                state.breaker.record_success_and_maybe_close(duration);
                if !state.breaker.is_open() {
                    state.opened_at = None;
                }
                Ok(value)
            }
            Err(err) => {
                let was_open = state.breaker.is_open();
                state.breaker.record_failure_and_maybe_trip(duration);
                if !was_open && state.breaker.is_open() {
                    state.opened_at = Some(Instant::now());
                }
                Err(BreakerError::Operation(err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn opens_after_failure_and_rejects_without_running_operation() {
        let breaker = AsyncCircuitBreaker::new(
            "test",
            1,
            Duration::from_secs(60),
            Duration::from_secs(30),
            1,
        );
        let calls = AtomicUsize::new(0);

        let first = breaker
            .call(|| async {
                calls.fetch_add(1, Ordering::Relaxed);
                Err::<(), _>("down")
            })
            .await;
        assert!(matches!(first, Err(BreakerError::Operation("down"))));

        let second = breaker
            .call(|| async {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok::<(), &str>(())
            })
            .await;
        assert!(matches!(second, Err(BreakerError::Open)));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn timeout_allows_a_probe_that_closes_the_breaker() {
        let breaker = AsyncCircuitBreaker::new(
            "test-recovery",
            1,
            Duration::from_secs(60),
            Duration::from_millis(10),
            1,
        );

        let first = breaker.call(|| async { Err::<(), _>("down") }).await;
        assert!(matches!(first, Err(BreakerError::Operation("down"))));
        assert!(breaker.retry_after().is_some());

        tokio::time::sleep(Duration::from_millis(20)).await;
        let recovered = breaker.call(|| async { Ok::<_, &str>("up") }).await;

        assert!(matches!(recovered, Ok("up")));
        assert_eq!(breaker.retry_after(), None);
    }
}
