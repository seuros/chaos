use chaos_ipc::approvals::ElicitationRequest;
use pretty_assertions::assert_eq;

use super::*;

#[tokio::test]
async fn elicitation_suite() {
    approval_elicitation_response_maps_to_core_action_and_review_decision();
    unsupported_approval_elicitation_runs_denial_callback().await;
    forwarded_elicitation_preserves_request_fields();
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
    let outgoing = OutgoingMessageSender::new(
        std::sync::Weak::new(),
        "test".into(),
        None,
        Arc::new(crate::outgoing_message::UrlElicitations::default()),
    );
    let denied = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let denied_for_callback = std::sync::Arc::clone(&denied);

    let response = create_approval_elicitation_or_deny(
        &outgoing,
        RequestId::Number(1.into()),
        &serde_json::json!({"message": "approve?", "requestedSchema": {"type":"object"}}),
        "test approval",
        move || async move {
            denied_for_callback.store(true, std::sync::atomic::Ordering::Relaxed);
        },
    )
    .await;

    assert!(response.is_none());
    assert!(denied.load(std::sync::atomic::Ordering::Relaxed));
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
            ForwardedElicitationRequestParams::from_protocol_request(request),
            expected
        );
    }
}

#[test]
fn metadata_must_be_an_object() {
    assert!(matches!(decode_meta(None), Ok(None)));
    assert!(matches!(
        decode_meta(Some(serde_json::json!({"source":"inner"}))),
        Ok(Some(_))
    ));
    assert!(decode_meta(Some(serde_json::json!("not-an-object"))).is_err());
}
