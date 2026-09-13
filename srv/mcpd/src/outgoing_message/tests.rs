use std::path::PathBuf;

use super::*;
use anyhow::Result;
use chaos_ipc::ProcessId;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionConfiguredEvent;
use chaos_test_fixtures::TEST_MODEL;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn outgoing_message_suite() {
    outgoing_notification_serializes_as_jsonrpc_notification();
    test_send_event_as_notification()
        .await
        .expect("test_send_event_as_notification");
    test_send_event_as_notification_with_meta()
        .await
        .expect("test_send_event_as_notification_with_meta");
    test_send_event_as_notification_with_meta_and_process_id()
        .await
        .expect("test_send_event_as_notification_with_meta_and_process_id");
    empty_elicitation_capability_defaults_to_form_support();
}

fn outgoing_notification_serializes_as_jsonrpc_notification() {
    let msg: OutgoingJsonRpcMessage = OutgoingMessage::Notification(OutgoingNotification {
        method: "notifications/initialized".to_string(),
        params: None,
    })
    .into();

    let value = serde_json::to_value(msg).expect("message should serialize");
    let obj = value.as_object().expect("json object");

    assert_eq!(obj.get("jsonrpc"), Some(&json!("2.0")));
    assert_eq!(obj.get("method"), Some(&json!("notifications/initialized")));
    assert!(
        obj.get("params").is_none() || obj.get("params") == Some(&serde_json::Value::Null),
        "params should be absent or null"
    );
}

async fn test_send_event_as_notification() -> Result<()> {
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
    let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

    let process_id = ProcessId::new();
    let event = Event {
        id: "1".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: TEST_MODEL.to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: chaos_ipc::config_types::ApprovalsReviewer::User,
            vfs_policy: chaos_ipc::protocol::VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
            socket_policy: chaos_ipc::protocol::SocketPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: Some(ReasoningEffort::default()),
            history_log_id: 1,
            history_entry_count: 1000,
            initial_messages: None,
            network_proxy: None,
        }),
    };

    outgoing_message_sender
        .send_event_as_notification(&event, None)
        .await;

    let result = outgoing_rx.recv().await.unwrap();
    let OutgoingMessage::Notification(OutgoingNotification { method, params }) = result else {
        panic!("expected Notification for first message");
    };
    assert_eq!(method, "chaos/event");

    let Ok(expected_params) = serde_json::to_value(&event) else {
        panic!("Event must serialize");
    };
    assert_eq!(params, Some(expected_params));
    Ok(())
}

async fn test_send_event_as_notification_with_meta() -> Result<()> {
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
    let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

    let conversation_id = ProcessId::new();
    let session_configured_event = SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        process_name: None,
        model: TEST_MODEL.to_string(),
        model_provider_id: "test-provider".to_string(),
        service_tier: None,
        approval_policy: ApprovalPolicy::Headless,
        approvals_reviewer: chaos_ipc::config_types::ApprovalsReviewer::User,
        vfs_policy: chaos_ipc::protocol::VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
        socket_policy: chaos_ipc::protocol::SocketPolicy::from(
            &SandboxPolicy::new_read_only_policy(),
        ),
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffort::default()),
        history_log_id: 1,
        history_entry_count: 1000,
        initial_messages: None,
        network_proxy: None,
    };
    let event = Event {
        id: "1".to_string(),
        msg: EventMsg::SessionConfigured(session_configured_event.clone()),
    };
    let meta = OutgoingNotificationMeta {
        request_id: Some(RequestId::String("123".into())),
        process_id: None,
    };

    outgoing_message_sender
        .send_event_as_notification(&event, Some(meta))
        .await;

    let result = outgoing_rx.recv().await.unwrap();
    let OutgoingMessage::Notification(OutgoingNotification { method, params }) = result else {
        panic!("expected Notification for first message");
    };
    assert_eq!(method, "chaos/event");
    let expected_params = json!({
        "_meta": {
            "requestId": "123",
        },
        "id": "1",
        "msg": {
            "type": "session_configured",
            "session_id": session_configured_event.session_id,
            "model": TEST_MODEL,
            "model_provider_id": "test-provider",
            "approval_policy": "headless",
            "approvals_reviewer": "user",
            "vfs_policy": session_configured_event.vfs_policy,
            "socket_policy": session_configured_event.socket_policy,
            "cwd": "/home/user/project",
            "reasoning_effort": session_configured_event.reasoning_effort,
            "history_log_id": session_configured_event.history_log_id,
            "history_entry_count": session_configured_event.history_entry_count,
        }
    });
    assert_eq!(params.unwrap(), expected_params);
    Ok(())
}

async fn test_send_event_as_notification_with_meta_and_process_id() -> Result<()> {
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
    let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

    let process_id = ProcessId::new();
    let session_configured_event = SessionConfiguredEvent {
        session_id: process_id,
        forked_from_id: None,
        process_name: None,
        model: TEST_MODEL.to_string(),
        model_provider_id: "test-provider".to_string(),
        service_tier: None,
        approval_policy: ApprovalPolicy::Headless,
        approvals_reviewer: chaos_ipc::config_types::ApprovalsReviewer::User,
        vfs_policy: chaos_ipc::protocol::VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
        socket_policy: chaos_ipc::protocol::SocketPolicy::from(
            &SandboxPolicy::new_read_only_policy(),
        ),
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffort::default()),
        history_log_id: 1,
        history_entry_count: 1000,
        initial_messages: None,
        network_proxy: None,
    };
    let event = Event {
        id: "1".to_string(),
        msg: EventMsg::SessionConfigured(session_configured_event.clone()),
    };
    let meta = OutgoingNotificationMeta {
        request_id: Some(RequestId::String("123".into())),
        process_id: Some(process_id),
    };

    outgoing_message_sender
        .send_event_as_notification(&event, Some(meta))
        .await;

    let result = outgoing_rx.recv().await.unwrap();
    let OutgoingMessage::Notification(OutgoingNotification { method, params }) = result else {
        panic!("expected Notification for first message");
    };
    assert_eq!(method, "chaos/event");
    let expected_params = json!({
        "_meta": {
            "requestId": "123",
            "processId": process_id.to_string(),
        },
        "id": "1",
        "msg": {
            "type": "session_configured",
            "session_id": session_configured_event.session_id,
            "model": TEST_MODEL,
            "model_provider_id": "test-provider",
            "approval_policy": "headless",
            "approvals_reviewer": "user",
            "vfs_policy": session_configured_event.vfs_policy,
            "socket_policy": session_configured_event.socket_policy,
            "cwd": "/home/user/project",
            "reasoning_effort": session_configured_event.reasoning_effort,
            "history_log_id": session_configured_event.history_log_id,
            "history_entry_count": session_configured_event.history_entry_count,
        }
    });
    assert_eq!(params.unwrap(), expected_params);
    Ok(())
}

fn empty_elicitation_capability_defaults_to_form_support() {
    let (outgoing_tx, _outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
    let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

    outgoing_message_sender
        .set_client_elicitation_capability(Some(&ElicitationCapability::default()));

    assert!(outgoing_message_sender.supports_form_elicitation());
}
