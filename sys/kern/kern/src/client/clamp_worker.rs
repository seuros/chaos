//! Native inference stays owned until its subprocess has been reaped.
use std::sync::atomic::Ordering;

use rama::futures::FutureExt;
use rama::futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::ModelClient;
use crate::client_common::ResponseEvent;
use crate::error::{ChaosErr, Result};

type Event = std::result::Result<ResponseEvent, chaos_parrot::error::ApiError>;

#[derive(Debug)]
pub(super) struct ClampWorker {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<Result<()>>,
    outcome: Option<std::result::Result<(), String>>,
}

impl ClampWorker {
    async fn wait(&mut self) -> Result<()> {
        if let Some(outcome) = &self.outcome {
            return outcome.clone().map_err(ChaosErr::Fatal);
        }
        let outcome = match (&mut self.task).await {
            Ok(result) => result.map_err(|error| error.to_string()),
            Err(_) => Err("native inference worker failed".into()),
        };
        self.outcome
            .insert(outcome)
            .clone()
            .map_err(ChaosErr::Fatal)
    }
}

impl ModelClient {
    pub(super) async fn spawn_clamp_worker(
        &self,
        output: mpsc::Sender<Event>,
        mut events: mpsc::Receiver<Event>,
        worker: BoxFuture<'static, ()>,
    ) -> Result<()> {
        let mut slot = self.state.clamp_worker.lock().await;
        if let Some(previous) = slot.as_mut() {
            if !previous.task.is_finished() {
                return Err(ChaosErr::Fatal("native inference is still active".into()));
            }
            previous.wait().await?;
        }
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let client = self.clone();
        self.state.clamp_pending.store(true, Ordering::SeqCst);
        let task = tokio::spawn(async move {
            // Buffer the terminal event: neither accounting nor the turn owner
            // may observe completion before native containment is confirmed.
            let forward = async {
                let mut terminal = None;
                while let Some(event) = events.recv().await {
                    if event.is_err() || matches!(event, Ok(ResponseEvent::Completed { .. })) {
                        terminal = Some(event);
                    } else if output.send(event).await.is_err() {
                        break;
                    }
                }
                terminal.or_else(|| {
                    Some(Err(chaos_parrot::error::ApiError::InvalidRequest {
                        message: "native inference ended without a result".into(),
                    }))
                })
            };
            let terminal = {
                let run = async { tokio::join!(worker, forward).1 };
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => None,
                    _ = output.closed() => None,
                    result = std::panic::AssertUnwindSafe(run).catch_unwind() => {
                        result.unwrap_or_else(|_| Some(Err(
                            chaos_parrot::error::ApiError::InvalidRequest {
                                message: "native inference worker failed".into(),
                            }
                        )))
                    }
                }
            };
            client.reap_clamp_transports().await?;
            client.state.clamp_pending.store(false, Ordering::SeqCst);
            if let Some(event) = terminal {
                tokio::select! {
                    _ = stop.cancelled() => {},
                    _ = output.send(event) => {},
                }
            }
            Ok(())
        });
        *slot = Some(ClampWorker {
            cancel,
            task,
            outcome: None,
        });
        Ok(())
    }

    /// Cancellation is not containment: wait for the worker's cleanup, retaining
    /// its handle and pending flag on failure. Does not erase resume checkpoints.
    pub(crate) async fn stop_clamp_worker(&self) -> Result<()> {
        let mut slot = self.state.clamp_worker.lock().await;
        if let Some(worker) = slot.as_mut() {
            worker.cancel.cancel();
            worker.wait().await?;
            *slot = None;
        }
        self.reap_clamp_transports().await
    }

    pub(crate) fn clamp_is_pending(&self) -> bool {
        self.state.clamp_pending.load(Ordering::SeqCst)
    }

    async fn reap_clamp_transports(&self) -> Result<()> {
        let mut claude = self.state.clamp_transport.lock().await;
        if let Some(transport) = claude.as_mut() {
            transport
                .terminate()
                .await
                .map_err(|error| ChaosErr::Fatal(format!("native containment failed: {error}")))?;
            *claude = None;
        }
        drop(claude);
        let mut agy = self.state.antigravity_transport.lock().await;
        if let Some(transport) = agy.as_mut() {
            transport
                .terminate()
                .await
                .map_err(|error| ChaosErr::Fatal(format!("native containment failed: {error}")))?;
        }
        Ok(())
    }
}
