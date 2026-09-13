use chaos_ipc::approvals::ElicitationRequest;
use mcp_host::protocol::capabilities::ElicitationCapability;
use mcp_host::protocol::capabilities::UrlElicitationCapability;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc;

use super::*;
use crate::outgoing_message::OutgoingMessage;

#[tokio::test]
async fn elicitation_suite() {
    approval_elicitation_response_maps_to_core_action_and_review_decision();
    unsupported_approval_elicitation_runs_denial_callback().await;
    forwarded_elicitation_preserves_request_fields();
    url_elicitation_support_requires_declared_url_capability();
    forwarded_url_elicitation_completion_sends_notification().await;
}

fn approval_elicitation_response_maps_to_core_action_and_review_decision() {
    let response = ApprovalElicitationResponse {
        action: ApprovalElicitationAction::Cancel,
        content: None,
        meta: None,
    };

    assert_eq!(response.core_action(), CoreElicitationAction::Cancel);
    assert_eq!(response.review_decision(), ReviewDecision::Denied);
}

async fn unsupported_approval_elicitation_runs_denial_callback() {
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
    let outgoing = OutgoingMessageSender::new(outgoing_tx);
    let denied = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let denied_for_callback = std::sync::Arc::clone(&denied);

    let response = create_approval_elicitation_or_deny(
        &outgoing,
        RequestId::Number(1.into()),
        &serde_json::json!({"message": "approve?"}),
        "test approval",
        move || async move {
            denied_for_callback.store(true, std::sync::atomic::Ordering::Relaxed);
        },
    )
    .await;

    assert!(response.is_none());
    assert!(denied.load(std::sync::atomic::Ordering::Relaxed));
    assert!(outgoing_rx.try_recv().is_err());
}

fn forwarded_elicitation_preserves_request_fields() {
    for (request, expected) in [
        (
            ElicitationRequest::Form {
                meta: Some(serde_json::json!({ "source": "inner-server" })),
                message: "Need confirmation".to_string(),
                requested_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": { "type": "boolean" }
                    }
                }),
            },
            ForwardedElicitationRequestParams::Form {
                meta: Some(serde_json::json!({ "source": "inner-server" })),
                message: "Need confirmation".to_string(),
                requested_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": { "type": "boolean" }
                    }
                }),
            },
        ),
        (
            ElicitationRequest::Url {
                meta: Some(serde_json::json!({ "flow": "oauth" })),
                message: "Complete sign-in".to_string(),
                url: "https://example.test/connect".to_string(),
                elicitation_id: "elicit-123".to_string(),
            },
            ForwardedElicitationRequestParams::Url {
                meta: Some(serde_json::json!({ "flow": "oauth" })),
                message: "Complete sign-in".to_string(),
                url: "https://example.test/connect".to_string(),
                elicitation_id: "elicit-123".to_string(),
            },
        ),
    ] {
        assert_eq!(
            ForwardedElicitationRequestParams::from_protocol_request(&request),
            expected
        );
    }
}

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
