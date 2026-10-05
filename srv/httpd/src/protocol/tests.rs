use super::*;
use chaos_test_fixtures::TEST_MODEL;

#[test]
fn trigger_request_accepts_canonical_fields_and_aliases() {
    for (label, json, request, session_id) in [
        (
            "request field",
            r#"{"request": "hello world"}"#,
            "hello world",
            None,
        ),
        (
            "prompt alias",
            r#"{"prompt": "hello world"}"#,
            "hello world",
            None,
        ),
        (
            "session_id alias",
            r#"{"request": "x", "session_id": "abc-123"}"#,
            "x",
            Some("abc-123"),
        ),
    ] {
        let req: TriggerRequest = serde_json::from_str(json).expect(label);
        assert_eq!(req.request.as_deref(), Some(request), "{label}: request");
        assert_eq!(
            req.caller_session_id.as_deref(),
            session_id,
            "{label}: caller session"
        );
    }
}

#[test]
fn trigger_request_rejects_model_field_detected() {
    let json = serde_json::json!({"request": "x", "model": TEST_MODEL});
    let req: TriggerRequest = serde_json::from_value(json).unwrap();
    assert!(req.model.is_some());
}

#[test]
fn empty_request_is_none() {
    let json = r#"{}"#;
    let req: TriggerRequest = serde_json::from_str(json).unwrap();
    assert!(req.request.is_none());
}

#[test]
fn failure_responses_preserve_wire_status_and_omit_absent_ids() {
    for (label, response, message) in [
        (
            "error",
            ApiErrorResponse::error("unauthorized"),
            "unauthorized",
        ),
        (
            "timeout",
            ApiErrorResponse::timeout("deadline exceeded"),
            "deadline exceeded",
        ),
    ] {
        let json = serde_json::to_value(&response).expect(label);
        assert_eq!(
            json,
            serde_json::json!({"status": label, "error": message}),
            "{label}: absent identifiers must be omitted, not null"
        );
    }
}
