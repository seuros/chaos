use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use anyhow::Result;
use anyhow::anyhow;
use chaos_ipc::protocol::ApprovalPolicy;
use mcp_guest::protocol::ElicitationResponse;
use mcp_guest::protocol::RequestId;
use tokio::sync::Mutex;
use tokio::sync::oneshot;

pub(super) type ResponderMap = HashMap<(String, RequestId), oneshot::Sender<ElicitationResponse>>;

pub(super) fn elicitation_is_rejected_by_policy(approval_policy: ApprovalPolicy) -> bool {
    match approval_policy {
        ApprovalPolicy::Headless => true,
        ApprovalPolicy::Interactive | ApprovalPolicy::Supervised => false,
        ApprovalPolicy::Granular(granular_config) => !granular_config.allows_mcp_elicitations(),
    }
}

#[derive(Clone)]
pub(super) struct ElicitationRequestManager {
    pub(super) requests: Arc<Mutex<ResponderMap>>,
    pub(super) approval_policy: Arc<StdMutex<ApprovalPolicy>>,
}

impl ElicitationRequestManager {
    pub(super) fn new(approval_policy: ApprovalPolicy) -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            approval_policy: Arc::new(StdMutex::new(approval_policy)),
        }
    }

    pub(super) async fn resolve(
        &self,
        server_name: String,
        id: RequestId,
        response: ElicitationResponse,
    ) -> Result<()> {
        self.requests
            .lock()
            .await
            .remove(&(server_name, id))
            .ok_or_else(|| anyhow!("elicitation request not found"))?
            .send(response)
            .map_err(|e| anyhow!("failed to send elicitation response: {e:?}"))
    }
}
