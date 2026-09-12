//! UI polling of the same bounded collector used for fresh model/resource checks.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chaos_kern::machine_status::{MachineStatus, ObservationRequest};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub(super) type Snapshot = Option<Arc<MachineStatus>>;
pub(super) type Source = watch::Receiver<Snapshot>;
pub(super) type Context = watch::Receiver<Option<ObservationRequest>>;

pub(super) struct Monitor {
    pub(super) source: Source,
    task: JoinHandle<()>,
}

impl Monitor {
    pub(super) fn new(context: Context) -> Self {
        Self::start(context, |request| async move { request.observe().await })
    }

    fn start<F, Fut>(mut context: Context, mut observe: F) -> Self
    where
        F: FnMut(ObservationRequest) -> Fut + Send + 'static,
        Fut: Future<Output = Result<MachineStatus, String>> + Send,
    {
        let (publisher, source) = watch::channel(None);
        let task = tokio::spawn(async move {
            loop {
                let request = context.borrow_and_update().clone();
                let configured = request.is_some();
                if let Some(request) = request {
                    tokio::select! {
                        biased;
                        changed = context.changed() => {
                            // Never publish a late result for the previous workspace.
                            publisher.send_replace(None);
                            if changed.is_err() {
                                break;
                            }
                            continue;
                        }
                        result = observe(request) => {
                            let snapshot = match result {
                                Ok(status) => Some(Arc::new(status)),
                                Err(error) => {
                                    tracing::debug!(%error, "top bar machine check unavailable");
                                    None // Do not keep stale percentages after a failed check.
                                }
                            };
                            publisher.send_replace(snapshot);
                        }
                    }
                }
                tokio::select! {
                    changed = context.changed() => {
                        publisher.send_replace(None);
                        if changed.is_err() {
                            break;
                        }
                    }
                    // No catch-up bursts after suspend; one read at a time.
                    _ = tokio::time::sleep(Duration::from_secs(30)), if configured => {}
                }
            }
        });
        Self { source, task }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests;
