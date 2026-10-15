//! Optional server-owned accounting at the logical provider-stream boundary.
use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_journald::{InvocationReceipt, InvocationStart};
use futures::future::BoxFuture;

#[derive(Debug, Default)]
pub(crate) struct AdmissionEpoch {
    generation: std::sync::atomic::AtomicU64,
    writers: std::sync::atomic::AtomicUsize,
    metered_processes: std::sync::Mutex<std::collections::HashSet<ProcessId>>,
}

impl AdmissionEpoch {
    pub(crate) fn record_metered(&self, process: ProcessId) -> crate::error::Result<()> {
        self.metered_processes
            .lock()
            .map_err(|_| {
                crate::error::ChaosErr::Fatal("metered process ownership unavailable".into())
            })?
            .insert(process);
        Ok(())
    }

    pub(crate) fn was_metered(&self, process: ProcessId) -> crate::error::Result<bool> {
        Ok(self
            .metered_processes
            .lock()
            .map_err(|_| {
                crate::error::ChaosErr::Fatal("metered process ownership unavailable".into())
            })?
            .contains(&process))
    }

    pub(crate) fn snapshot(&self) -> Option<u64> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.writers.load(SeqCst) != 0 {
            return None;
        }
        let generation = self.generation.load(SeqCst);
        (self.writers.load(SeqCst) == 0).then_some(generation)
    }

    pub(crate) fn mutate(self: &Arc<Self>) -> crate::error::Result<AdmissionMutation> {
        use std::sync::atomic::Ordering::SeqCst;
        self.writers
            .try_update(SeqCst, SeqCst, |writers| writers.checked_add(1))
            .map_err(|_| {
                crate::error::ChaosErr::Fatal("descendant admission writers exhausted".into())
            })?;
        if self
            .generation
            .try_update(SeqCst, SeqCst, |g| g.checked_add(1))
            .is_err()
        {
            // Exhaustion is permanently fenced rather than wrapping to an old epoch.
            return Err(crate::error::ChaosErr::Fatal(
                "descendant admission epoch exhausted".into(),
            ));
        }
        Ok(AdmissionMutation(self.clone()))
    }
}

pub(crate) struct AdmissionMutation(Arc<AdmissionEpoch>);
impl Drop for AdmissionMutation {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::SeqCst;
        if self
            .0
            .generation
            .try_update(SeqCst, SeqCst, |g| g.checked_add(1))
            .is_ok()
        {
            self.0.writers.fetch_sub(1, SeqCst);
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("provider invocation accounting unavailable")]
pub struct InvocationAccountingError;

/// A successful, newly committed begin is required before provider dispatch.
/// This does not identify hidden HTTP retries or promise complete billing.
pub trait ProviderInvocationSink: std::fmt::Debug + Send + Sync {
    /// Admit an actual descendant before its model client or startup loop can
    /// dispatch. Implementations must durably authorize lineage under the root
    /// execution lease and return a sink bound to that root turn and child.
    /// Unimplemented lineage is rejected, never silently made unmetered.
    fn fork(
        &self,
        _parent_turn_id: uuid::Uuid,
        _parent_process_id: ProcessId,
        _child_process_id: ProcessId,
    ) -> BoxFuture<'_, Result<Arc<dyn ProviderInvocationSink>, InvocationAccountingError>> {
        Box::pin(async { Err(InvocationAccountingError) })
    }

    fn begin(
        &self,
        request: InvocationStart,
    ) -> BoxFuture<'_, Result<bool, InvocationAccountingError>>;
    fn complete(
        &self,
        invocation_id: uuid::Uuid,
        receipt: InvocationReceipt,
    ) -> BoxFuture<'_, Result<(), InvocationAccountingError>>;
}

/// Internal authority, not derived from a copied transcript or client input.
#[derive(Clone, Debug)]
pub(crate) struct InvocationParent {
    sink: Arc<dyn ProviderInvocationSink>,
    parent_turn_id: uuid::Uuid,
    parent_process_id: ProcessId,
}

impl InvocationParent {
    pub(crate) fn new(
        sink: Arc<dyn ProviderInvocationSink>,
        parent_turn_id: uuid::Uuid,
        parent_process_id: ProcessId,
    ) -> Self {
        Self {
            sink,
            parent_turn_id,
            parent_process_id,
        }
    }

    pub(crate) async fn admit(
        self,
        child_process_id: ProcessId,
    ) -> Result<Arc<dyn ProviderInvocationSink>, InvocationAccountingError> {
        if child_process_id == self.parent_process_id {
            return Err(InvocationAccountingError);
        }
        self.sink
            .fork(
                self.parent_turn_id,
                self.parent_process_id,
                child_process_id,
            )
            .await
    }
}
