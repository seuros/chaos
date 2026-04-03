use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::FileChange;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::ReviewDecision;
use chaos_kern::Process;
use mcp_host::protocol::types::RequestId;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use tracing::error;

use crate::elicitation::ApprovalElicitationResponse;
use crate::elicitation::CreateFormElicitationError;
use crate::elicitation::create_form_elicitation_request;
use crate::elicitation::decode_approval_elicitation_response;
use crate::outgoing_message::OutgoingMessageSender;

#[derive(Debug, Deserialize, Serialize)]
pub struct PatchApprovalElicitRequestParams {
    pub message: String,
    #[serde(rename = "requestedSchema")]
    pub requested_schema: Value,
    #[serde(rename = "_meta")]
    pub meta: PatchApprovalElicitRequestMeta,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PatchApprovalElicitRequestMeta {
    #[serde(rename = "processId")]
    pub process_id: ProcessId,
    pub codex_elicitation: String,
    pub codex_mcp_tool_call_id: String,
    pub codex_event_id: String,
    pub codex_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_grant_root: Option<PathBuf>,
    pub codex_changes: HashMap<PathBuf, FileChange>,
}

pub type PatchApprovalResponse = ApprovalElicitationResponse;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_patch_approval_request(
    call_id: String,
    reason: Option<String>,
    grant_root: Option<PathBuf>,
    changes: HashMap<PathBuf, FileChange>,
    outgoing: Arc<OutgoingMessageSender>,
    codex: Arc<Process>,
    request_id: RequestId,
    tool_call_id: String,
    event_id: String,
    process_id: ProcessId,
) {
    let approval_id = call_id.clone();
    let mut message_lines = Vec::new();
    if let Some(r) = &reason {
        message_lines.push(r.clone());
    }
    message_lines.push("Allow Chaos to apply proposed code changes?".to_string());

    let params = PatchApprovalElicitRequestParams {
        message: message_lines.join("\n"),
        requested_schema: json!({"type":"object","properties":{}}),
        meta: PatchApprovalElicitRequestMeta {
            process_id,
            codex_elicitation: "patch-approval".to_string(),
            codex_mcp_tool_call_id: tool_call_id.clone(),
            codex_event_id: event_id.clone(),
            codex_call_id: call_id,
            codex_reason: reason,
            codex_grant_root: grant_root,
            codex_changes: changes,
        },
    };
    let on_response = match create_form_elicitation_request(
        outgoing.as_ref(),
        request_id.clone(),
        &params,
        "PatchApprovalElicitRequestParams",
    )
    .await
    {
        Ok(receiver) => receiver,
        Err(CreateFormElicitationError::InvalidParams) => return,
        Err(CreateFormElicitationError::Unsupported) => {
            submit_patch_approval(approval_id, ReviewDecision::Denied, codex).await;
            return;
        }
    };

    // Listen for the response on a separate task so we don't block the main agent loop.
    {
        let codex = codex.clone();
        let approval_id = approval_id.clone();
        tokio::spawn(async move {
            on_patch_approval_response(approval_id, on_response, codex).await;
        });
    }
}

pub(crate) async fn on_patch_approval_response(
    approval_id: String,
    receiver: crate::elicitation::ElicitationResponseReceiver,
    codex: Arc<Process>,
) {
    let response = decode_approval_elicitation_response(receiver, "PatchApprovalResponse").await;

    submit_patch_approval(approval_id, response.review_decision(), codex).await;
}

async fn submit_patch_approval(approval_id: String, decision: ReviewDecision, codex: Arc<Process>) {
    if let Err(err) = codex
        .submit(Op::PatchApproval {
            id: approval_id,
            decision,
        })
        .await
    {
        error!("failed to submit PatchApproval: {err}");
    }
}
