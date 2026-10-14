//! Optional server-owned accounting at the logical provider-stream boundary.
use chaos_journald::{InvocationReceipt, InvocationStart};
use futures::future::BoxFuture;

#[derive(Debug, thiserror::Error)]
#[error("provider invocation accounting unavailable")]
pub struct InvocationAccountingError;

/// A successful, newly committed begin is required before provider dispatch.
/// This does not identify hidden HTTP retries or promise complete billing.
pub trait ProviderInvocationSink: std::fmt::Debug + Send + Sync {
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
