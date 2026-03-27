use std::sync::Arc;

use crate::outgoing_message::ErrorData;
use chaos_kern::Process;
use chaos_ipc::approvals::ElicitationAction as CoreElicitationAction;
use chaos_ipc::approvals::ElicitationRequest as CoreElicitationRequest;
use chaos_ipc::approvals::ElicitationRequestEvent;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::ReviewDecision;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tracing::error;

use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingNotification;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalElicitationAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ApprovalElicitationResponse {
    pub action: ApprovalElicitationAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl ApprovalElicitationResponse {
    pub(crate) fn review_decision(&self) -> ReviewDecision {
        match self.action {
            ApprovalElicitationAction::Accept => ReviewDecision::Approved,
            ApprovalElicitationAction::Decline | ApprovalElicitationAction::Cancel => {
                ReviewDecision::Denied
            }
        }
    }

    pub(crate) fn core_action(&self) -> CoreElicitationAction {
        match self.action {
            ApprovalElicitationAction::Accept => CoreElicitationAction::Accept,
            ApprovalElicitationAction::Decline => CoreElicitationAction::Decline,
            ApprovalElicitationAction::Cancel => CoreElicitationAction::Cancel,
        }
    }

    fn decline() -> Self {
        Self {
            action: ApprovalElicitationAction::Decline,
            content: None,
            meta: None,
        }
    }

    fn cancel() -> Self {
        Self {
            action: ApprovalElicitationAction::Cancel,
            content: None,
            meta: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum ForwardedElicitationRequestParams {
    Form {
        #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
        message: String,
        #[serde(rename = "requestedSchema")]
        requested_schema: Value,
    },
    Url {
        #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
        message: String,
        url: String,
        #[serde(rename = "elicitationId")]
        elicitation_id: String,
    },
}

impl ForwardedElicitationRequestParams {
    fn from_protocol_request(request: &CoreElicitationRequest) -> Self {
        match request {
            CoreElicitationRequest::Form {
                meta,
                message,
                requested_schema,
            } => Self::Form {
                meta: meta.clone(),
                message: message.clone(),
                requested_schema: requested_schema.clone(),
            },
            CoreElicitationRequest::Url {
                meta,
                message,
                url,
                elicitation_id,
            } => Self::Url {
                meta: meta.clone(),
                message: message.clone(),
                url: url.clone(),
                elicitation_id: elicitation_id.clone(),
            },
        }
    }

    fn is_supported_by(&self, outgoing: &OutgoingMessageSender) -> bool {
        match self {
            Self::Form { .. } => outgoing.supports_form_elicitation(),
            Self::Url { .. } => outgoing.supports_url_elicitation(),
        }
    }
}

pub(crate) async fn handle_mcp_server_elicitation_request(
    request: ElicitationRequestEvent,
    outgoing: Arc<OutgoingMessageSender>,
    codex: Arc<Process>,
) {
    let params = ForwardedElicitationRequestParams::from_protocol_request(&request.request);
    if !params.is_supported_by(outgoing.as_ref()) {
        error!(
            server_name = request.server_name,
            request_id = ?request.id,
            "client does not support requested elicitation mode; cancelling request"
        );
        submit_resolve_elicitation(
            request.server_name,
            request.id,
            ApprovalElicitationResponse::cancel(),
            codex,
        )
        .await;
        return;
    }

    let params_json = match serde_json::to_value(&params) {
        Ok(value) => value,
        Err(err) => {
            error!(
                error = %err,
                server_name = request.server_name,
                request_id = ?request.id,
                "failed to serialize elicitation request"
            );
            submit_resolve_elicitation(
                request.server_name,
                request.id,
                ApprovalElicitationResponse::cancel(),
                codex,
            )
            .await;
            return;
        }
    };

    let server_name = request.server_name.clone();
    let request_id = request.id.clone();
    let on_response = outgoing
        .send_request("elicitation/create", Some(params_json))
        .await;

    tokio::spawn(async move {
        on_mcp_server_elicitation_response(server_name, request_id, on_response, codex).await;
    });
}

pub(crate) async fn handle_mcp_server_elicitation_complete(
    elicitation_id: String,
    outgoing: Arc<OutgoingMessageSender>,
) {
    outgoing
        .send_notification(OutgoingNotification {
            method: "notifications/elicitation/complete".to_string(),
            params: Some(serde_json::json!({
                "elicitationId": elicitation_id,
            })),
        })
        .await;
}

async fn on_mcp_server_elicitation_response(
    server_name: String,
    request_id: chaos_ipc::mcp::RequestId,
    receiver: tokio::sync::oneshot::Receiver<Result<Value, ErrorData>>,
    codex: Arc<Process>,
) {
    let response = match receiver.await {
        Ok(Ok(value)) => serde_json::from_value::<ApprovalElicitationResponse>(value)
            .unwrap_or_else(|err| {
                error!(
                    error = %err,
                    server_name,
                    request_id = ?request_id,
                    "failed to deserialize elicitation response"
                );
                ApprovalElicitationResponse::decline()
            }),
        Ok(Err(err)) => {
            error!(
                error = ?err,
                server_name,
                request_id = ?request_id,
                "elicitation request failed with client error"
            );
            ApprovalElicitationResponse::decline()
        }
        Err(err) => {
            error!(
                error = ?err,
                server_name,
                request_id = ?request_id,
                "elicitation request failed"
            );
            ApprovalElicitationResponse::decline()
        }
    };

    submit_resolve_elicitation(server_name, request_id, response, codex).await;
}

async fn submit_resolve_elicitation(
    server_name: String,
    request_id: chaos_ipc::mcp::RequestId,
    response: ApprovalElicitationResponse,
    codex: Arc<Process>,
) {
    if let Err(err) = codex
        .submit(Op::ResolveElicitation {
            server_name,
            request_id,
            decision: response.core_action(),
            content: response.content,
            meta: response.meta,
        })
        .await
    {
        error!("failed to submit ResolveElicitation: {err}");
    }
}

#[cfg(test)]
mod tests {
    use chaos_ipc::approvals::ElicitationRequest;
    use mcp_host::protocol::capabilities::ElicitationCapability;
    use mcp_host::protocol::capabilities::UrlElicitationCapability;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc;

    use super::*;
    use crate::outgoing_message::OutgoingMessage;

    #[test]
    fn approval_elicitation_response_maps_to_core_action_and_review_decision() {
        let response = ApprovalElicitationResponse {
            action: ApprovalElicitationAction::Cancel,
            content: None,
            meta: None,
        };

        assert_eq!(response.core_action(), CoreElicitationAction::Cancel);
        assert_eq!(response.review_decision(), ReviewDecision::Denied);
    }

    #[test]
    fn forwarded_form_elicitation_preserves_meta_and_schema() {
        let request = ElicitationRequest::Form {
            meta: Some(serde_json::json!({ "source": "inner-server" })),
            message: "Need confirmation".to_string(),
            requested_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "confirmed": { "type": "boolean" }
                }
            }),
        };

        assert_eq!(
            ForwardedElicitationRequestParams::from_protocol_request(&request),
            ForwardedElicitationRequestParams::Form {
                meta: Some(serde_json::json!({ "source": "inner-server" })),
                message: "Need confirmation".to_string(),
                requested_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": { "type": "boolean" }
                    }
                }),
            }
        );
    }

    #[test]
    fn forwarded_url_elicitation_preserves_url_fields() {
        let request = ElicitationRequest::Url {
            meta: Some(serde_json::json!({ "flow": "oauth" })),
            message: "Complete sign-in".to_string(),
            url: "https://example.test/connect".to_string(),
            elicitation_id: "elicit-123".to_string(),
        };

        assert_eq!(
            ForwardedElicitationRequestParams::from_protocol_request(&request),
            ForwardedElicitationRequestParams::Url {
                meta: Some(serde_json::json!({ "flow": "oauth" })),
                message: "Complete sign-in".to_string(),
                url: "https://example.test/connect".to_string(),
                elicitation_id: "elicit-123".to_string(),
            }
        );
    }

    #[test]
    fn url_elicitation_support_requires_declared_url_capability() {
        let (outgoing_tx, _outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let outgoing = OutgoingMessageSender::new(outgoing_tx);
        outgoing.set_client_elicitation_capability(Some(&ElicitationCapability {
            form: None,
            url: Some(UrlElicitationCapability::default()),
        }));

        let request = ForwardedElicitationRequestParams::Url {
            meta: None,
            message: "Complete sign-in".to_string(),
            url: "https://example.test/connect".to_string(),
            elicitation_id: "elicit-123".to_string(),
        };

        assert!(request.is_supported_by(&outgoing));
    }

    #[tokio::test]
    async fn forwarded_url_elicitation_completion_sends_notification() {
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let outgoing = Arc::new(OutgoingMessageSender::new(outgoing_tx));

        handle_mcp_server_elicitation_complete("elicit-123".to_string(), outgoing).await;

        let Some(OutgoingMessage::Notification(notification)) = outgoing_rx.recv().await else {
            panic!("expected notification");
        };

        assert_eq!(notification.method, "notifications/elicitation/complete");
        assert_eq!(
            notification.params,
            Some(serde_json::json!({
                "elicitationId": "elicit-123",
            }))
        );
    }
}
