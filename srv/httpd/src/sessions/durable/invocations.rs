use chaos_journald::{ExecutionInvocationRequest, InvocationReceipt, InvocationStart};
use chaos_kern::invocation::{InvocationAccountingError, ProviderInvocationSink};
use std::future::Future;
use std::pin::Pin;

use super::*;

struct Sink(Arc<Owner>);
impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvocationSink")
            .field("session", &self.0.id)
            .finish()
    }
}

impl ProviderInvocationSink for Sink {
    fn begin(
        &self,
        request: InvocationStart,
    ) -> Pin<Box<dyn Future<Output = Result<bool, InvocationAccountingError>> + Send + '_>> {
        Box::pin(async move {
            self.0
                .check_execution()
                .map_err(|_| InvocationAccountingError)?;
            let lease = self.0.lease().map_err(|_| InvocationAccountingError)?;
            match self
                .0
                .write(Request::Invocation {
                    request: ExecutionInvocationRequest::Begin { lease, request },
                })
                .await
            {
                Ok(Response::InvocationBegan { newly_recorded, .. }) => Ok(newly_recorded),
                _ => Err(InvocationAccountingError),
            }
        })
    }
    fn complete(
        &self,
        invocation_id: Uuid,
        receipt: InvocationReceipt,
    ) -> Pin<Box<dyn Future<Output = Result<(), InvocationAccountingError>> + Send + '_>> {
        Box::pin(async move {
            let lease = self.0.lease().map_err(|_| InvocationAccountingError)?;
            match self
                .0
                .write(Request::Invocation {
                    request: ExecutionInvocationRequest::Complete {
                        lease,
                        invocation_id,
                        receipt,
                    },
                })
                .await
            {
                Ok(Response::Invocation(_)) => Ok(()),
                _ => Err(InvocationAccountingError),
            }
        })
    }
}

impl Owner {
    pub(crate) fn invocation_sink(self: &Arc<Self>) -> Arc<dyn ProviderInvocationSink> {
        Arc::new(Sink(self.clone()))
    }
    pub(crate) async fn accounting(
        &self,
        turn_id: Uuid,
    ) -> Result<chaos_journald::ExecutionAccounting, ApiError> {
        match self
            .store
            .call(Request::Invocation {
                request: ExecutionInvocationRequest::Read {
                    session_id: self.id,
                    turn_id,
                },
            })
            .await?
        {
            Response::Accounting(accounting) => Ok(accounting),
            _ => Err(unavailable()),
        }
    }
}
