use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct Stopwatch {
    limit: Option<Duration>,
    inner: Arc<Mutex<StopwatchState>>,
    notify: Arc<Notify>,
}

#[derive(Debug)]
struct StopwatchState {
    elapsed: Duration,
    running_since: Option<Instant>,
    active_pauses: u32,
}

impl Stopwatch {
    pub fn new(limit: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(StopwatchState {
                elapsed: Duration::ZERO,
                running_since: Some(Instant::now()),
                active_pauses: 0,
            })),
            notify: Arc::new(Notify::new()),
            limit: Some(limit),
        }
    }

    pub fn unlimited() -> Self {
        Self {
            inner: Arc::new(Mutex::new(StopwatchState {
                elapsed: Duration::ZERO,
                running_since: Some(Instant::now()),
                active_pauses: 0,
            })),
            notify: Arc::new(Notify::new()),
            limit: None,
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let Some(limit) = self.limit else {
            return token;
        };
        let cancel = token.clone();
        let inner = Arc::clone(&self.inner);
        let notify = Arc::clone(&self.notify);
        tokio::spawn(async move {
            loop {
                let (remaining, running) = {
                    let guard = inner.lock().await;
                    let elapsed = guard.elapsed
                        + guard
                            .running_since
                            .map(|since| since.elapsed())
                            .unwrap_or_default();
                    if elapsed >= limit {
                        break;
                    }
                    (limit - elapsed, guard.running_since.is_some())
                };

                if !running {
                    notify.notified().await;
                    continue;
                }

                let sleep = tokio::time::sleep(remaining);
                tokio::pin!(sleep);
                tokio::select! {
                    _ = &mut sleep => {
                        break;
                    }
                    _ = notify.notified() => {
                        continue;
                    }
                }
            }
            cancel.cancel();
        });
        token
    }

    /// Runs `fut`, pausing the stopwatch while the future is pending. The clock
    /// resumes automatically when the future completes. Nested/overlapping
    /// calls are reference-counted so the stopwatch only resumes when every
    /// pause is lifted.
    pub async fn pause_for<F, T>(&self, fut: F) -> T
    where
        F: Future<Output = T>,
    {
        self.pause().await;
        let result = fut.await;
        self.resume().await;
        result
    }

    async fn pause(&self) {
        let mut guard = self.inner.lock().await;
        guard.active_pauses += 1;
        if guard.active_pauses == 1
            && let Some(since) = guard.running_since.take()
        {
            guard.elapsed += since.elapsed();
            self.notify.notify_waiters();
        }
    }

    async fn resume(&self) {
        let mut guard = self.inner.lock().await;
        if guard.active_pauses == 0 {
            return;
        }
        guard.active_pauses -= 1;
        if guard.active_pauses == 0 && guard.running_since.is_none() {
            guard.running_since = Some(Instant::now());
            self.notify.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests;
