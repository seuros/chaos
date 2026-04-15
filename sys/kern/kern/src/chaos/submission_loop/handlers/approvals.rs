use std::sync::Arc;

use chaos_ipc::mcp::RequestId as ProtocolRequestId;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ReviewDecision;
use chaos_ipc::protocol::WarningEvent;
use chaos_ipc::request_permissions::RequestPermissionsResponse;
use chaos_ipc::request_user_input::RequestUserInputResponse;
use chaos_mcp_runtime::ElicitationAction;
use chaos_mcp_runtime::ElicitationResponse;
use serde_json::Value;
use tracing::warn;

use crate::chaos::Session;

pub async fn exec_approval(
    sess: &Arc<Session>,
    approval_id: String,
    turn_id: Option<String>,
    decision: ReviewDecision,
) {
    let event_turn_id = turn_id.unwrap_or_else(|| approval_id.clone());
    if let ReviewDecision::ApprovedExecpolicyAmendment {
        proposed_execpolicy_amendment,
    } = &decision
    {
        match sess
            .persist_execpolicy_amendment(proposed_execpolicy_amendment)
            .await
        {
            Ok(()) => {
                sess.record_execpolicy_amendment_message(
                    &event_turn_id,
                    proposed_execpolicy_amendment,
                )
                .await;
            }
            Err(err) => {
                let message = format!("Failed to apply execpolicy amendment: {err}");
                tracing::warn!("{message}");
                let warning = EventMsg::Warning(WarningEvent { message });
                sess.send_event_raw(Event {
                    id: event_turn_id.clone(),
                    msg: warning,
                })
                .await;
            }
        }
    }
    match decision {
        ReviewDecision::Abort => {
            sess.interrupt_task().await;
        }
        other => sess.notify_exec_approval(&approval_id, other).await,
    }
}

pub async fn patch_approval(sess: &Arc<Session>, id: String, decision: ReviewDecision) {
    match decision {
        ReviewDecision::Abort => {
            sess.interrupt_task().await;
        }
        other => sess.notify_patch_approval(&id, other).await,
    }
}

pub async fn request_user_input_response(
    sess: &Arc<Session>,
    id: String,
    response: RequestUserInputResponse,
) {
    sess.notify_user_input_response(&id, response).await;
}

pub async fn request_permissions_response(
    sess: &Arc<Session>,
    id: String,
    response: RequestPermissionsResponse,
) {
    sess.notify_request_permissions_response(&id, response)
        .await;
}

pub async fn resolve_elicitation(
    sess: &Arc<Session>,
    server_name: String,
    request_id: ProtocolRequestId,
    decision: chaos_ipc::approvals::ElicitationAction,
    content: Option<Value>,
    meta: Option<Value>,
) {
    let action = match decision {
        chaos_ipc::approvals::ElicitationAction::Accept => ElicitationAction::Accept,
        chaos_ipc::approvals::ElicitationAction::Decline => ElicitationAction::Decline,
        chaos_ipc::approvals::ElicitationAction::Cancel => ElicitationAction::Cancel,
    };
    let content = match action {
        // Preserve the legacy fallback for clients that only send an action.
        ElicitationAction::Accept => Some(content.unwrap_or_else(|| serde_json::json!({}))),
        ElicitationAction::Decline | ElicitationAction::Cancel => None,
    };
    let response = ElicitationResponse {
        action,
        content,
        meta,
    };
    let request_id = chaos_mcp_runtime::manager::protocol_request_id_to_guest(&request_id);
    if let Err(err) = sess
        .resolve_elicitation(server_name, request_id, response)
        .await
    {
        warn!(
            error = %err,
            "failed to resolve elicitation request in session"
        );
    }
}
