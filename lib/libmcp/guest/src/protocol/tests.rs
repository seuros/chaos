use super::*;

#[test]
fn test_supported_versions_cover_the_negotiable_range() {
    assert_eq!(SUPPORTED_PROTOCOL_VERSIONS, ["2025-11-25", "2025-06-18"]);
    assert_eq!(latest_supported_protocol_version(), "2025-11-25");
    assert!(is_supported_protocol_version("2025-11-25"));
    assert!(is_supported_protocol_version("2025-06-18"));
    assert!(!is_supported_protocol_version("2026-07-28"));
    assert!(!is_supported_protocol_version("2025-03-26"));
}

#[test]
fn test_call_tool_result_allows_structured_content_without_blocks() {
    let result: CallToolResult = serde_json::from_value(serde_json::json!({
        "structuredContent": {
            "answer": 42
        }
    }))
    .unwrap();

    assert!(result.content.is_empty());
    assert_eq!(result.structured_content.unwrap()["answer"], 42);
}

#[test]
fn test_resource_link_uses_resource_link_tag() {
    let content = ContentBlock::ResourceLink {
        uri: "file:///tmp/example".to_string(),
        name: "example".to_string(),
        title: None,
        description: None,
        mime_type: None,
        size: None,
        icons: None,
        annotations: None,
        meta: None,
    };

    let json = serde_json::to_value(content).unwrap();
    assert_eq!(json["type"], "resource_link");
}

#[test]
fn test_create_message_result_accepts_current_shape() {
    let result: CreateMessageResult = serde_json::from_value(serde_json::json!({
        "role": "assistant",
        "content": {
            "type": "text",
            "text": "hello"
        },
        "model": "test-model",
        "stopReason": "endTurn"
    }))
    .unwrap();

    assert_eq!(result.role, Role::Assistant);
    assert_eq!(result.stop_reason.as_deref(), Some("endTurn"));
}

#[test]
fn test_url_elicitation_request_deserializes() {
    let request: CreateElicitationRequest = serde_json::from_value(serde_json::json!({
        "mode": "url",
        "message": "Authorize access",
        "elicitationId": "auth-1",
        "url": "https://example.com/auth"
    }))
    .unwrap();

    match request {
        CreateElicitationRequest::Url(params) => {
            assert_eq!(params.elicitation_id, "auth-1");
        }
        CreateElicitationRequest::Form(_) => panic!("expected url request"),
    }
}

#[test]
fn test_completion_result_accepts_string_values() {
    let result: CompleteResult = serde_json::from_value(serde_json::json!({
        "completion": {
            "values": ["alpha", "beta"],
            "hasMore": false
        }
    }))
    .unwrap();

    assert_eq!(result.completion.values, vec!["alpha", "beta"]);
}

#[test]
fn test_completion_result_accepts_legacy_object_values() {
    let result: CompleteResult = serde_json::from_value(serde_json::json!({
        "completion": {
            "values": [
                { "value": "alpha", "label": "Alpha" },
                { "value": "beta" }
            ]
        }
    }))
    .unwrap();

    assert_eq!(result.completion.values, vec!["alpha", "beta"]);
}

#[test]
fn test_server_capabilities_accepts_legacy_completion_field() {
    let capabilities: ServerCapabilities = serde_json::from_value(serde_json::json!({
        "completion": {}
    }))
    .unwrap();

    assert!(capabilities.completions.is_some());
}

#[test]
fn test_cancelled_notification_allows_missing_request_id() {
    let params: CancelledNotificationParams = serde_json::from_value(serde_json::json!({
        "reason": "server shutdown"
    }))
    .unwrap();

    assert!(params.request_id.is_none());
    assert_eq!(params.reason.as_deref(), Some("server shutdown"));
}

#[test]
fn test_call_tool_response_accepts_task_result() {
    let response: CallToolResponse = serde_json::from_value(serde_json::json!({
        "task": {
            "taskId": "task-123",
            "status": "working",
            "createdAt": "2026-03-24T00:00:00Z",
            "lastUpdatedAt": "2026-03-24T00:00:00Z",
            "ttl": null
        }
    }))
    .unwrap();

    let task = response.into_task().expect("expected task result");
    assert_eq!(task.task.task_id, "task-123");
}
