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
        ApprovalPolicy::Headless | ApprovalPolicy::Interactive | ApprovalPolicy::Supervised => {
            false
        }
        ApprovalPolicy::Granular(granular_config) => !granular_config.allows_mcp_elicitations(),
    }
}

/// Only answer a confirmation, never a URL or a request for user data.
pub(super) fn approval_content(
    request: &mcp_guest::protocol::CreateElicitationRequest,
) -> Option<serde_json::Value> {
    let mcp_guest::protocol::CreateElicitationRequest::Form(form) = request else {
        return None;
    };
    let schema = &form.requested_schema;
    let properties = schema["properties"].as_object()?;
    let (key, field) = properties.iter().next()?;
    (schema["type"] == "object"
        && properties.len() == 1
        && matches!(key.as_str(), "approve" | "confirm")
        && field["type"] == "boolean"
        && schema.get("required").is_none_or(|r| {
            r.as_array().is_some_and(|fields| {
                fields.is_empty() || (fields.len() == 1 && fields[0] == key.as_str())
            })
        }))
    .then(|| serde_json::json!({(key): true}))
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
