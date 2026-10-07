//! Single-owner preparation. A caller deadline is not permission to detach
//! blocking I/O and start another attempt on top of it.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub(super) const TIMEOUT: Duration = Duration::from_secs(300);
type ResultChannel<T> = watch::Receiver<Option<Result<T, String>>>;

pub(super) struct Preparation<T> {
    state: Arc<Mutex<State<T>>>,
    timeout: Duration,
}

struct State<T> {
    ready: Option<T>,
    running: Option<ResultChannel<T>>,
}

impl<T: Clone + Send + Sync + 'static> Preparation<T> {
    pub(super) fn new(timeout: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                ready: None,
                running: None,
            })),
            timeout,
        }
    }

    pub(super) fn ready(&self) -> Option<T> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ready
            .clone()
    }

    pub(super) async fn prepare<F, Fut>(&self, loader: F) -> Result<T, String>
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
    {
        let mut receiver = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ready) = &state.ready {
                return Ok(ready.clone());
            }
            if let Some(running) = &state.running {
                running.clone()
            } else {
                let (sender, receiver) = watch::channel(None);
                state.running = Some(receiver.clone());
                let state = self.state.clone();
                let timeout = self.timeout;
                // The worker, not its callers, owns this attempt and cancellation.
                tokio::spawn(async move {
                    let cancellation = CancellationToken::new();
                    let child_cancel = cancellation.clone();
                    let mut worker = tokio::spawn(async move { loader(child_cancel).await });
                    let result = tokio::select! {
                        biased;
                        _ = tokio::time::sleep(timeout) => {
                            cancellation.cancel();
                            // Bound every waiter, but retain ownership until SDK I/O
                            // and model loading have drained. Never abort this join.
                            sender.send_replace(Some(Err(
                                "Recall preparation exceeded 300 seconds; cleanup is draining. \
                                 Retry enable_tools after cleanup completes.".into()
                            )));
                            let _ = worker.await;
                            None
                        }
                        result = &mut worker => Some(result.unwrap_or_else(|_| {
                            Err("Recall preparation worker failed to finish".into())
                        })),
                    };
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(result) = result {
                        if let Ok(ready) = &result {
                            state.ready = Some(ready.clone());
                        }
                        sender.send_replace(Some(result));
                    }
                    // Existing waiters retain their result channel. Only a later
                    // explicit preparation can create a fresh attempt after failure.
                    state.running = None;
                });
                receiver
            }
        };
        receiver
            .wait_for(Option::is_some)
            .await
            .map_err(|_| "Recall preparation stopped before becoming ready".to_owned())?
            .clone()
            .ok_or_else(|| "Recall preparation has no result".to_owned())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn startup_is_lazy_and_concurrent_success_is_shared() {
        let runtime = Preparation::new(TIMEOUT);
        assert!(runtime.ready().is_none());
        let calls = Arc::new(AtomicUsize::new(0));
        let loader = || {
            let calls = calls.clone();
            move |_| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(Arc::new(7))
            }
        };
        let (first, second) = tokio::join!(runtime.prepare(loader()), runtime.prepare(loader()));
        let first = first.unwrap();
        let second = second.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let cached = runtime
            .prepare(|_| async { panic!("success must not load again") })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&first, &cached));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failure_is_shared_but_later_explicit_retry_succeeds() {
        let runtime = Preparation::<()>::new(TIMEOUT);
        let calls = Arc::new(AtomicUsize::new(0));
        let loader = || {
            let calls = calls.clone();
            move |_| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Err("test failure".into())
            }
        };
        let (first, second) = tokio::join!(runtime.prepare(loader()), runtime.prepare(loader()));
        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(runtime.ready().is_none());
        runtime.prepare(|_| async { Ok(()) }).await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_waiter_does_not_cancel_or_duplicate_worker() {
        let runtime = Arc::new(Preparation::new(TIMEOUT));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let first = runtime.clone();
        let waiter = tokio::spawn(async move {
            first
                .prepare(move |_| async {
                    started_tx.send(()).unwrap();
                    finish_rx.await.unwrap();
                    Ok(7)
                })
                .await
        });
        started_rx.await.unwrap();
        waiter.abort();
        let _ = waiter.await;
        let second = runtime.prepare(|_| async { panic!("must join worker") });
        finish_tx.send(()).unwrap();
        assert_eq!(second.await.unwrap(), 7);
    }

    #[tokio::test]
    async fn timeout_retains_ownership_until_cleanup_drains_and_never_publishes_late_success() {
        let runtime = Arc::new(Preparation::new(Duration::from_millis(20)));
        let (draining_tx, draining_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let first = runtime.clone();
        let waiter = tokio::spawn(async move {
            first
                .prepare(move |cancellation| async move {
                    cancellation.cancelled().await;
                    draining_tx.send(()).unwrap();
                    finish_rx.await.unwrap();
                    Ok(7)
                })
                .await
        });
        let error = waiter.await.unwrap().unwrap_err();
        assert!(error.contains("cleanup"));
        draining_rx.await.unwrap();
        assert_eq!(
            runtime
                .prepare(|_| async { panic!("no overlapping attempt") })
                .await
                .unwrap_err(),
            error
        );
        assert!(runtime.ready().is_none());
        finish_tx.send(()).unwrap();
        // Observe actual cleanup rather than assume a fixed scheduling delay.
        loop {
            if runtime.state.lock().unwrap().running.is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(runtime.ready().is_none());
        assert_eq!(runtime.prepare(|_| async { Ok(8) }).await.unwrap(), 8);
    }
}
