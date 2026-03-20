use std::collections::HashMap;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use codex_protocol::ThreadId;
use codex_protocol::protocol::Event;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tracing::warn;

use crate::mcp_types::*;

pub(crate) type OutgoingJsonRpcMessage = JsonRpcMessage;

/// Sends messages to the client and manages request callbacks.
pub(crate) struct OutgoingMessageSender {
    next_request_id: AtomicI64,
    sender: mpsc::UnboundedSender<OutgoingMessage>,
    request_id_to_callback: Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, ErrorData>>>>,
    client_elicitation_modes: AtomicU8,
}

impl OutgoingMessageSender {
    pub(crate) fn new(sender: mpsc::UnboundedSender<OutgoingMessage>) -> Self {
        Self {
            next_request_id: AtomicI64::new(0),
            sender,
            request_id_to_callback: Mutex::new(HashMap::new()),
            client_elicitation_modes: AtomicU8::new(0),
        }
    }

    pub(crate) async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> oneshot::Receiver<Result<Value, ErrorData>> {
        let id = RequestId::Number(self.next_request_id.fetch_add(1, Ordering::Relaxed));
        let outgoing_message_id = id.clone();
        let (tx_approve, rx_approve) = oneshot::channel();
        {
            let mut request_id_to_callback = self.request_id_to_callback.lock().await;
            request_id_to_callback.insert(id, tx_approve);
        }

        let outgoing_message = OutgoingMessage::Request(OutgoingRequest {
            id: outgoing_message_id,
            method: method.to_string(),
            params,
        });
        let _ = self.sender.send(outgoing_message);
        rx_approve
    }

    pub(crate) async fn notify_client_response(&self, id: RequestId, result: Value) {
        let entry = {
            let mut request_id_to_callback = self.request_id_to_callback.lock().await;
            request_id_to_callback.remove_entry(&id)
        };

        match entry {
            Some((id, sender)) => {
                if let Err(err) = sender.send(Ok(result)) {
                    warn!("could not notify callback for {id:?} due to: {err:?}");
                }
            }
            None => {
                warn!("could not find callback for {id:?}");
            }
        }
    }

    pub(crate) async fn notify_client_error(&self, id: RequestId, error: ErrorData) {
        let entry = {
            let mut request_id_to_callback = self.request_id_to_callback.lock().await;
            request_id_to_callback.remove_entry(&id)
        };

        match entry {
            Some((id, sender)) => {
                if let Err(err) = sender.send(Err(error)) {
                    warn!("could not notify callback for {id:?} due to: {err:?}");
                }
            }
            None => {
                warn!("could not find callback for {id:?}");
            }
        }
    }

    pub(crate) fn set_client_elicitation_capability(
        &self,
        elicitation: Option<&ElicitationCapability>,
    ) {
        const FORM_MODE: u8 = 0b01;
        const URL_MODE: u8 = 0b10;

        let supports_form = match elicitation {
            None => false,
            Some(capability) if capability.form.is_some() => true,
            Some(capability) => capability.url.is_none(),
        };
        let supports_url = elicitation.is_some_and(|capability| capability.url.is_some());

        let mut modes = 0;
        if supports_form {
            modes |= FORM_MODE;
        }
        if supports_url {
            modes |= URL_MODE;
        }
        self.client_elicitation_modes
            .store(modes, Ordering::Relaxed);
    }

    pub(crate) fn supports_form_elicitation(&self) -> bool {
        self.client_elicitation_modes.load(Ordering::Relaxed) & 0b01 != 0
    }

    pub(crate) fn supports_url_elicitation(&self) -> bool {
        self.client_elicitation_modes.load(Ordering::Relaxed) & 0b10 != 0
    }

    pub(crate) async fn send_response<T: Serialize>(&self, id: RequestId, response: T) {
        let result = match serde_json::to_value(response) {
            Ok(result) => result,
            Err(err) => {
                self.send_error(
                    id,
                    ErrorData::internal_error(format!("failed to serialize response: {err}")),
                )
                .await;
                return;
            }
        };

        let outgoing_message = OutgoingMessage::Response(OutgoingResponse { id, result });
        let _ = self.sender.send(outgoing_message);
    }

    /// This is used with the MCP server, but not the more general JSON-RPC app
    /// server. Prefer [`OutgoingMessageSender::send_server_notification`] where
    /// possible.
    pub(crate) async fn send_event_as_notification(
        &self,
        event: &Event,
        meta: Option<OutgoingNotificationMeta>,
    ) {
        #[expect(clippy::expect_used)]
        let event_json = serde_json::to_value(event).expect("Event must serialize");

        let params = if let Ok(params) = serde_json::to_value(OutgoingNotificationParams {
            meta,
            event: event_json.clone(),
        }) {
            params
        } else {
            warn!("Failed to serialize event as OutgoingNotificationParams");
            event_json
        };

        self.send_notification(OutgoingNotification {
            method: "codex/event".to_string(),
            params: Some(params.clone()),
        })
        .await;
    }

    pub(crate) async fn send_notification(&self, notification: OutgoingNotification) {
        let outgoing_message = OutgoingMessage::Notification(notification);
        let _ = self.sender.send(outgoing_message);
    }

    pub(crate) async fn send_error(&self, id: RequestId, error: ErrorData) {
        let outgoing_message = OutgoingMessage::Error(OutgoingError { id, error });
        let _ = self.sender.send(outgoing_message);
    }
}

/// Outgoing message from the server to the client.
pub(crate) enum OutgoingMessage {
    Request(OutgoingRequest),
    Notification(OutgoingNotification),
    Response(OutgoingResponse),
    Error(OutgoingError),
}

impl From<OutgoingMessage> for OutgoingJsonRpcMessage {
    fn from(val: OutgoingMessage) -> Self {
        use OutgoingMessage::*;
        match val {
            Request(OutgoingRequest { id, method, params }) => {
                JsonRpcMessage::Request(JsonRpcRequest::new(id.to_value(), method, params))
            }
            Notification(OutgoingNotification { method, params }) => {
                JsonRpcMessage::Notification(JsonRpcRequest::notification(method, params))
            }
            Response(OutgoingResponse { id, result }) => {
                JsonRpcMessage::Response(JsonRpcResponse::success(id.to_value(), result))
            }
            Error(OutgoingError { id, error }) => {
                JsonRpcMessage::Response(JsonRpcResponse::error(id.to_value(), error))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OutgoingRequest {
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OutgoingNotification {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OutgoingNotificationParams {
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<OutgoingNotificationMeta>,

    #[serde(flatten)]
    pub event: serde_json::Value,
}

// Additional mcp-specific data to be added to a [`codex_protocol::protocol::Event`] as notification.params._meta
// MCP Spec: https://modelcontextprotocol.io/specification/2025-06-18/basic#meta
// Typescript Schema: https://github.com/modelcontextprotocol/modelcontextprotocol/blob/0695a497eb50a804fc0e88c18a93a21a675d6b3e/schema/2025-06-18/schema.ts
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OutgoingNotificationMeta {
    pub request_id: Option<RequestId>,

    /// Because multiple threads may be multiplexed over a single MCP connection,
    /// include the `threadId` in the notification meta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OutgoingResponse {
    pub id: RequestId,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OutgoingError {
    pub error: ErrorData,
    pub id: RequestId,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::Result;
    use codex_protocol::ThreadId;
    use codex_protocol::openai_models::ReasoningEffort;
    use codex_protocol::protocol::AskForApproval;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::SandboxPolicy;
    use codex_protocol::protocol::SessionConfiguredEvent;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn outgoing_request_serializes_as_jsonrpc_request() {
        let msg: OutgoingJsonRpcMessage = OutgoingMessage::Request(OutgoingRequest {
            id: RequestId::Number(1),
            method: "elicitation/create".to_string(),
            params: Some(json!({ "k": "v" })),
        })
        .into();

        let value = serde_json::to_value(msg).expect("message should serialize");
        let obj = value.as_object().expect("json object");

        assert_eq!(obj.get("jsonrpc"), Some(&json!("2.0")));
        assert_eq!(obj.get("id"), Some(&json!(1)));
        assert_eq!(obj.get("method"), Some(&json!("elicitation/create")));
        assert_eq!(obj.get("params"), Some(&json!({ "k": "v" })));
    }

    #[test]
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
        // mcp-host omits null params via skip_serializing_if
        assert!(
            obj.get("params").is_none() || obj.get("params") == Some(&serde_json::Value::Null),
            "params should be absent or null"
        );
    }

    #[tokio::test]
    async fn test_send_event_as_notification() -> Result<()> {
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

        let thread_id = ThreadId::new();
        let rollout_file = NamedTempFile::new()?;
        let event = Event {
            id: "1".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: thread_id,
                forked_from_id: None,
                thread_name: None,
                model: "gpt-4o".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: codex_protocol::config_types::ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: Some(ReasoningEffort::default()),
                history_log_id: 1,
                history_entry_count: 1000,
                initial_messages: None,
                network_proxy: None,
                rollout_path: Some(rollout_file.path().to_path_buf()),
            }),
        };

        outgoing_message_sender
            .send_event_as_notification(&event, None)
            .await;

        let result = outgoing_rx.recv().await.unwrap();
        let OutgoingMessage::Notification(OutgoingNotification { method, params }) = result else {
            panic!("expected Notification for first message");
        };
        assert_eq!(method, "codex/event");

        let Ok(expected_params) = serde_json::to_value(&event) else {
            panic!("Event must serialize");
        };
        assert_eq!(params, Some(expected_params));
        Ok(())
    }

    #[tokio::test]
    async fn test_send_event_as_notification_with_meta() -> Result<()> {
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

        let conversation_id = ThreadId::new();
        let rollout_file = NamedTempFile::new()?;
        let session_configured_event = SessionConfiguredEvent {
            session_id: conversation_id,
            forked_from_id: None,
            thread_name: None,
            model: "gpt-4o".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: codex_protocol::config_types::ApprovalsReviewer::User,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: Some(ReasoningEffort::default()),
            history_log_id: 1,
            history_entry_count: 1000,
            initial_messages: None,
            network_proxy: None,
            rollout_path: Some(rollout_file.path().to_path_buf()),
        };
        let event = Event {
            id: "1".to_string(),
            msg: EventMsg::SessionConfigured(session_configured_event.clone()),
        };
        let meta = OutgoingNotificationMeta {
            request_id: Some(RequestId::String("123".into())),
            thread_id: None,
        };

        outgoing_message_sender
            .send_event_as_notification(&event, Some(meta))
            .await;

        let result = outgoing_rx.recv().await.unwrap();
        let OutgoingMessage::Notification(OutgoingNotification { method, params }) = result else {
            panic!("expected Notification for first message");
        };
        assert_eq!(method, "codex/event");
        let expected_params = json!({
            "_meta": {
                "requestId": "123",
            },
            "id": "1",
            "msg": {
                "type": "session_configured",
                "session_id": session_configured_event.session_id,
                "model": "gpt-4o",
                "model_provider_id": "test-provider",
                "approval_policy": "never",
                "approvals_reviewer": "user",
                "sandbox_policy": {
                    "type": "read-only"
                },
                "cwd": "/home/user/project",
                "reasoning_effort": session_configured_event.reasoning_effort,
                "history_log_id": session_configured_event.history_log_id,
                "history_entry_count": session_configured_event.history_entry_count,
                "rollout_path": rollout_file.path().to_path_buf(),
            }
        });
        assert_eq!(params.unwrap(), expected_params);
        Ok(())
    }

    #[tokio::test]
    async fn test_send_event_as_notification_with_meta_and_thread_id() -> Result<()> {
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

        let thread_id = ThreadId::new();
        let rollout_file = NamedTempFile::new()?;
        let session_configured_event = SessionConfiguredEvent {
            session_id: thread_id,
            forked_from_id: None,
            thread_name: None,
            model: "gpt-4o".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: codex_protocol::config_types::ApprovalsReviewer::User,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: Some(ReasoningEffort::default()),
            history_log_id: 1,
            history_entry_count: 1000,
            initial_messages: None,
            network_proxy: None,
            rollout_path: Some(rollout_file.path().to_path_buf()),
        };
        let event = Event {
            id: "1".to_string(),
            msg: EventMsg::SessionConfigured(session_configured_event.clone()),
        };
        let meta = OutgoingNotificationMeta {
            request_id: Some(RequestId::String("123".into())),
            thread_id: Some(thread_id),
        };

        outgoing_message_sender
            .send_event_as_notification(&event, Some(meta))
            .await;

        let result = outgoing_rx.recv().await.unwrap();
        let OutgoingMessage::Notification(OutgoingNotification { method, params }) = result else {
            panic!("expected Notification for first message");
        };
        assert_eq!(method, "codex/event");
        let expected_params = json!({
            "_meta": {
                "requestId": "123",
                "threadId": thread_id.to_string(),
            },
            "id": "1",
            "msg": {
                "type": "session_configured",
                "session_id": session_configured_event.session_id,
                "model": "gpt-4o",
                "model_provider_id": "test-provider",
                "approval_policy": "never",
                "approvals_reviewer": "user",
                "sandbox_policy": {
                    "type": "read-only"
                },
                "cwd": "/home/user/project",
                "reasoning_effort": session_configured_event.reasoning_effort,
                "history_log_id": session_configured_event.history_log_id,
                "history_entry_count": session_configured_event.history_entry_count,
                "rollout_path": rollout_file.path().to_path_buf(),
            }
        });
        assert_eq!(params.unwrap(), expected_params);
        Ok(())
    }

    #[tokio::test]
    async fn send_request_callback_receives_client_error() -> Result<()> {
        let (outgoing_tx, _outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

        let response = outgoing_message_sender
            .send_request("elicitation/create", Some(json!({})))
            .await;

        outgoing_message_sender
            .notify_client_error(
                RequestId::Number(0),
                ErrorData::invalid_params("bad elicitation"),
            )
            .await;

        assert_eq!(
            response.await?,
            Err(ErrorData::invalid_params("bad elicitation"))
        );
        Ok(())
    }

    #[test]
    fn empty_elicitation_capability_defaults_to_form_support() {
        let (outgoing_tx, _outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

        outgoing_message_sender
            .set_client_elicitation_capability(Some(&ElicitationCapability::default()));

        assert!(outgoing_message_sender.supports_form_elicitation());
    }
}
