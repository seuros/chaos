use std::sync::Arc;

use chaos_ipc::config_types::{ReasoningSummary, ServiceTier};
use chaos_ipc::openai_models::{ModelInfo, ReasoningEffort};
use chaos_journald::{InvocationPurpose, InvocationReceipt, InvocationStart, InvocationTokenUsage};
use chaos_snitch::SessionTelemetry;
use rama::futures::StreamExt;

use super::{ModelClient, ModelClientSession};
use crate::client_common::{Prompt, ResponseEvent, ResponseStream};
use crate::error::{ChaosErr, Result};
use crate::invocation::ProviderInvocationSink;

fn unavailable() -> ChaosErr {
    ChaosErr::Fatal("provider invocation accounting unavailable; execution must reconcile".into())
}

fn runnable(op: &chaos_ipc::protocol::Op) -> bool {
    matches!(
        op,
        chaos_ipc::protocol::Op::UserTurn { .. }
            | chaos_ipc::protocol::Op::Compact
            | chaos_ipc::protocol::Op::Review { .. }
            | chaos_ipc::protocol::Op::RunUserShellCommand { .. }
    )
}

pub(crate) struct PendingSubmission(ModelClient);
impl Drop for PendingSubmission {
    fn drop(&mut self) {
        self.0.release_submission();
    }
}

impl ModelClient {
    pub(crate) fn bind_admission_epoch(
        &self,
        epoch: Arc<crate::invocation::AdmissionEpoch>,
    ) -> Result<()> {
        let mut installed = self
            .state
            .admission_epoch
            .lock()
            .map_err(|_| unavailable())?;
        if installed
            .as_ref()
            .is_some_and(|old| !Arc::ptr_eq(old, &epoch))
        {
            return Err(unavailable());
        }
        *installed = Some(epoch.clone());
        drop(installed);
        if self.has_invocation_sink()? {
            epoch.record_metered(self.state.conversation_id)?;
        }
        Ok(())
    }

    pub(crate) fn advance_admission_epoch(
        &self,
    ) -> Result<Option<crate::invocation::AdmissionMutation>> {
        let epoch = self
            .state
            .admission_epoch
            .lock()
            .map_err(|_| unavailable())?;
        epoch
            .as_ref()
            .map(crate::invocation::AdmissionEpoch::mutate)
            .transpose()
    }

    pub(crate) fn reserve_submission(&self, op: &chaos_ipc::protocol::Op) -> Result<bool> {
        if !runnable(op) || !self.has_invocation_sink()? {
            return Ok(false);
        }
        let _mutation = self.advance_admission_epoch()?;
        self.state
            .pending_submissions
            .try_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |count| {
                    (count < chaos_journald::MAX_EXECUTION_INVOCATIONS as usize).then(|| count + 1)
                },
            )
            .map_err(|_| unavailable())?;
        Ok(true)
    }

    pub(crate) fn release_submission(&self) {
        let Ok(_mutation) = self.advance_admission_epoch() else {
            return;
        };
        let _ = self.state.pending_submissions.try_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |count| Some(count.saturating_sub(1)),
        );
    }

    pub(crate) fn dispatch_submission(
        &self,
        op: &chaos_ipc::protocol::Op,
    ) -> Option<PendingSubmission> {
        (runnable(op) && self.has_pending_submissions()).then(|| PendingSubmission(self.clone()))
    }

    pub(crate) fn has_pending_submissions(&self) -> bool {
        self.state
            .pending_submissions
            .load(std::sync::atomic::Ordering::SeqCst)
            != 0
    }

    pub(crate) fn clear_pending_submissions(&self) {
        let Ok(_mutation) = self.advance_admission_epoch() else {
            return;
        };
        self.state
            .pending_submissions
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn invocation_parent(
        &self,
        turn_id: &str,
    ) -> Result<Option<crate::invocation::InvocationParent>> {
        let sink = self
            .state
            .invocation_sink
            .lock()
            .map_err(|_| unavailable())?
            .clone();
        sink.map(|sink| {
            let turn_id = uuid::Uuid::parse_str(turn_id).map_err(|_| unavailable())?;
            Ok(crate::invocation::InvocationParent::new(
                sink,
                turn_id,
                self.state.conversation_id,
            ))
        })
        .transpose()
    }

    pub(crate) fn has_invocation_sink(&self) -> Result<bool> {
        self.state
            .invocation_sink
            .lock()
            .map(|sink| sink.is_some())
            .map_err(|_| unavailable())
    }

    pub(crate) fn set_invocation_sink(&self, sink: Arc<dyn ProviderInvocationSink>) -> Result<()> {
        let mut installed = self
            .state
            .invocation_sink
            .lock()
            .map_err(|_| unavailable())?;
        if installed.is_some() {
            return Err(unavailable());
        }
        *installed = Some(sink);
        drop(installed);
        if let Some(epoch) = self
            .state
            .admission_epoch
            .lock()
            .map_err(|_| unavailable())?
            .as_ref()
        {
            epoch.record_metered(self.state.conversation_id)?;
        }
        Ok(())
    }
}

impl ModelClientSession {
    /// Non-metered callers must not accidentally bypass an installed sink.
    #[allow(clippy::too_many_arguments)]
    pub async fn stream(
        &mut self,
        prompt: &Prompt,
        model: &ModelInfo,
        telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffort>,
        summary: ReasoningSummary,
        tier: Option<ServiceTier>,
        metadata: Option<&str>,
    ) -> Result<ResponseStream> {
        if self.client.has_invocation_sink()? {
            return Err(unavailable());
        }
        self.stream_unmetered(prompt, model, telemetry, effort, summary, tier, metadata)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn stream_accounted(
        &mut self,
        turn_id: &str,
        purpose: InvocationPurpose,
        prompt: &Prompt,
        model: &ModelInfo,
        telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffort>,
        summary: ReasoningSummary,
        tier: Option<ServiceTier>,
        metadata: Option<&str>,
    ) -> Result<ResponseStream> {
        let sink = self
            .client
            .state
            .invocation_sink
            .lock()
            .map_err(|_| unavailable())?
            .clone();
        let Some(sink) = sink else {
            return self
                .stream_unmetered(prompt, model, telemetry, effort, summary, tier, metadata)
                .await;
        };
        if self
            .client
            .state
            .clamped
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(unavailable());
        }
        let start = InvocationStart {
            invocation_id: uuid::Uuid::new_v4(),
            turn_id: uuid::Uuid::parse_str(turn_id).map_err(|_| unavailable())?,
            process_id: self.client.state.conversation_id,
            provider_id: self.client.state.provider_id.clone(),
            requested_model: model.slug.clone(),
            requested_service_tier: tier,
            purpose,
        };
        let id = start.invocation_id;
        if !sink.begin(start).await.map_err(|_| unavailable())? {
            return Err(unavailable());
        }
        // Cancellation while opening can leave a started record. The execution
        // fence demotes it to unknown; it must never be interpreted as free.
        let stream = self
            .stream_unmetered(prompt, model, telemetry, effort, summary, tier, metadata)
            .await;
        match stream {
            Ok(stream) => Ok(account_response_stream(stream, sink, id)),
            Err(error) => {
                sink.complete(id, InvocationReceipt::unknown())
                    .await
                    .map_err(|_| unavailable())?;
                Err(error)
            }
        }
    }
}

pub(super) fn account_response_stream(
    mut stream: ResponseStream,
    sink: Arc<dyn ProviderInvocationSink>,
    id: uuid::Uuid,
) -> ResponseStream {
    let (tx_event, rx_event) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        let mut server_model = None;
        loop {
            let event = tokio::select! {
                _ = tx_event.closed() => None,
                event = stream.next() => event,
            };
            let Some(event) = event else {
                let _ = sink.complete(id, InvocationReceipt::unknown()).await;
                return;
            };
            match &event {
                Ok(ResponseEvent::ServerModel(model)) if !model.is_empty() => {
                    server_model = Some(model.clone());
                }
                Ok(ResponseEvent::Completed {
                    response_id,
                    token_usage,
                }) => {
                    let receipt = InvocationReceipt {
                        completed: true,
                        response_id: (!response_id.is_empty()).then(|| response_id.clone()),
                        server_model,
                        usage: token_usage.as_ref().map(InvocationTokenUsage::from),
                    };
                    // Forward completion only AFTER its immutable receipt commits.
                    let event = if sink.complete(id, receipt).await.is_ok() {
                        event
                    } else {
                        Err(unavailable())
                    };
                    let _ = tx_event.send(event).await;
                    return;
                }
                Err(_) => {
                    let event = if sink
                        .complete(id, InvocationReceipt::unknown())
                        .await
                        .is_ok()
                    {
                        event
                    } else {
                        Err(unavailable())
                    };
                    let _ = tx_event.send(event).await;
                    return;
                }
                _ => {}
            }
            if tx_event.send(event).await.is_err() {
                let _ = sink.complete(id, InvocationReceipt::unknown()).await;
                return;
            }
        }
    });
    ResponseStream { rx_event }
}

#[cfg(test)]
mod tests;
