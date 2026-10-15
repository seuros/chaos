use chaos_journald::{ExecutionInvocationRequest, InvocationReceipt, InvocationStart};
use chaos_kern::invocation::{InvocationAccountingError, ProviderInvocationSink};
use std::future::Future;
use std::pin::Pin;

use super::*;

struct Sink {
    owner: Arc<Owner>,
    lineage: Option<(Uuid, chaos_ipc::ProcessId)>,
}
impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvocationSink")
            .field("session", &self.owner.id)
            .field("lineage", &self.lineage)
            .finish()
    }
}

impl ProviderInvocationSink for Sink {
    fn fork(
        &self,
        parent_turn_id: Uuid,
        parent_process_id: chaos_ipc::ProcessId,
        child_process_id: chaos_ipc::ProcessId,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Arc<dyn ProviderInvocationSink>, InvocationAccountingError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            self.owner
                .check_execution()
                .map_err(|_| InvocationAccountingError)?;
            let root_turn_id = if let Some((root, process)) = self.lineage {
                if parent_process_id != process {
                    return Err(InvocationAccountingError);
                }
                root
            } else {
                parent_turn_id
            };
            let lease = self.owner.lease().map_err(|_| InvocationAccountingError)?;
            match self
                .owner
                .write(Request::Invocation {
                    request: ExecutionInvocationRequest::RegisterDescendant {
                        lease,
                        root_turn_id,
                        parent_process_id,
                        process_id: child_process_id,
                    },
                })
                .await
            {
                Ok(Response::DescendantRegistered {
                    newly_registered: true,
                }) => Ok(Arc::new(Sink {
                    owner: self.owner.clone(),
                    lineage: Some((root_turn_id, child_process_id)),
                }) as Arc<dyn ProviderInvocationSink>),
                _ => Err(InvocationAccountingError),
            }
        })
    }

    fn begin(
        &self,
        request: InvocationStart,
    ) -> Pin<Box<dyn Future<Output = Result<bool, InvocationAccountingError>> + Send + '_>> {
        Box::pin(async move {
            self.owner
                .check_execution()
                .map_err(|_| InvocationAccountingError)?;
            let lease = self.owner.lease().map_err(|_| InvocationAccountingError)?;
            let request = if let Some((root_turn_id, process)) = self.lineage {
                if request.process_id != process {
                    return Err(InvocationAccountingError);
                }
                ExecutionInvocationRequest::BeginDescendant {
                    lease,
                    root_turn_id,
                    request,
                }
            } else {
                ExecutionInvocationRequest::Begin { lease, request }
            };
            match self.owner.write(Request::Invocation { request }).await {
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
            let lease = self.owner.lease().map_err(|_| InvocationAccountingError)?;
            let request = if let Some((_, process_id)) = self.lineage {
                ExecutionInvocationRequest::CompleteDescendant {
                    lease,
                    process_id,
                    invocation_id,
                    receipt,
                }
            } else {
                ExecutionInvocationRequest::Complete {
                    lease,
                    invocation_id,
                    receipt,
                }
            };
            match self.owner.write(Request::Invocation { request }).await {
                Ok(Response::Invocation(_)) => Ok(()),
                _ => Err(InvocationAccountingError),
            }
        })
    }
}

impl Owner {
    pub(crate) fn invocation_sink(self: &Arc<Self>) -> Arc<dyn ProviderInvocationSink> {
        Arc::new(Sink {
            owner: self.clone(),
            lineage: None,
        })
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
