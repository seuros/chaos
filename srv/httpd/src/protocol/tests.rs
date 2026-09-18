use super::*;
use chaos_test_fixtures::TEST_MODEL;

#[test]
fn trigger_request_accepts_request_field() {
    let json = r#"{"request": "hello world"}"#;
    let req: TriggerRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.request.as_deref(), Some("hello world"));
}

#[test]
fn trigger_request_accepts_prompt_alias() {
    let json = r#"{"prompt": "hello world"}"#;
    let req: TriggerRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.request.as_deref(), Some("hello world"));
}

#[test]
fn trigger_request_accepts_session_id_alias() {
    let json = r#"{"request": "x", "session_id": "abc-123"}"#;
    let req: TriggerRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.caller_session_id.as_deref(), Some("abc-123"));
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
fn error_response_serializes_consistently() {
    let err = ApiErrorResponse::error("unauthorized");
    let json = serde_json::to_value(&err).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"], "unauthorized");
    // Optional fields should be absent, not null
    assert!(json.get("process_id").is_none());
}

#[test]
fn timeout_response_has_timeout_status() {
    let err = ApiErrorResponse::timeout("deadline exceeded");
    let json = serde_json::to_value(&err).unwrap();
    assert_eq!(json["status"], "timeout");
}
