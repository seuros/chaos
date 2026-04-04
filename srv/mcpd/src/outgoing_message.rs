use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::Event;
use mcp_host::protocol::capabilities::ElicitationCapability;
use mcp_host::protocol::types::JsonRpcError;
use mcp_host::protocol::types::JsonRpcMessage;
use mcp_host::protocol::types::JsonRpcRequest;
use mcp_host::protocol::types::JsonRpcResponse;
use mcp_host::protocol::types::RequestId;
use mcp_host::server::multiplexer::ClientRequester;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tracing::error;
use tracing::warn;

/// Alias kept for compatibility with existing call-sites.
pub(crate) type ErrorData = JsonRpcError;
pub(crate) type OutgoingJsonRpcMessage = JsonRpcMessage;

/// Routes outgoing messages to the MCP client.
///
/// Notifications (e.g. `codex/event`) are sent via the mcp-host notification
/// channel. Server→client requests (e.g. `elicitation/create` per MCP spec
/// §Elicitation) go through [`ClientRequester::request_raw`] so that mcp-host's
/// multiplexer can match responses back to their pending calls.
pub(crate) struct OutgoingMessageSender {
    sender: mpsc::UnboundedSender<OutgoingMessage>,
    /// Tracks which elicitation modes the client declared in `capabilities.elicitation`
    /// (MCP spec §Capabilities). Bit 0 = form, bit 1 = url.
    client_elicitation_modes: AtomicU8,
    /// Set once after `initialized` — required to send server→client requests.
    client_requester: Mutex<Option<ClientRequester>>,
}

impl OutgoingMessageSender {
    pub(crate) fn new(sender: mpsc::UnboundedSender<OutgoingMessage>) -> Self {
        Self {
            sender,
            client_elicitation_modes: AtomicU8::new(0),
            client_requester: Mutex::new(None),
        }
    }

    /// Called from the `on_initialized` hook once the client has completed the
    /// MCP handshake and its capabilities are known.
    pub(crate) async fn set_client_requester(&self, requester: ClientRequester) {
        *self.client_requester.lock().await = Some(requester);
    }

    /// Send a server→client request per the MCP spec and return a receiver for
    /// the client's response.
    ///
    /// Uses [`ClientRequester::request_raw`] so that:
    /// - The request is written to the transport with a unique JSON-RPC id.
    /// - The multiplexer routes the client's response back to the returned receiver.
    ///
    /// Chaos-specific extensions (e.g. `_meta` in `elicitation/create`) are allowed
    /// by the MCP spec's general `_meta` extension mechanism and are passed through
    /// unchanged in `params`.
    pub(crate) async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> oneshot::Receiver<Result<Value, ErrorData>> {
        let (tx, rx) = oneshot::channel();

        let guard = self.client_requester.lock().await;
        let Some(requester) = guard.as_ref() else {
            error!(
                method,
                "send_request called before ClientRequester was set; \
                 approval will be treated as denied"
            );
            return rx;
        };

        let requester = requester.clone();
        let method = method.to_string();

        tokio::spawn(async move {
            let result = requester.request_raw(&method, params, None).await;
            let mapped = result.map_err(|e| JsonRpcError {
                code: mcp_host::protocol::types::ErrorCode::INTERNAL_ERROR,
                message: e.to_string(),
                data: None,
            });
            let _ = tx.send(mapped);
        });

        rx
    }

    /// Mirror the client's declared `elicitation` capability so that approval
    /// handlers can check support without holding the requester lock.
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

    /// Encode a Chaos event as a `codex/event` MCP notification and enqueue it.
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
        let _ = self
            .sender
            .send(OutgoingMessage::Notification(notification));
    }

    pub(crate) async fn send_error(&self, id: RequestId, error: ErrorData) {
        let _ = self
            .sender
            .send(OutgoingMessage::Error(OutgoingError { id, error }));
    }
}

/// Outgoing message from the server to the MCP client sent via the notification
/// channel. Server→client *requests* (e.g. `elicitation/create`) bypass this
/// enum and go directly through `ClientRequester::request_raw`.
pub(crate) enum OutgoingMessage {
    Notification(OutgoingNotification),
    Error(OutgoingError),
}

impl From<OutgoingMessage> for OutgoingJsonRpcMessage {
    fn from(val: OutgoingMessage) -> Self {
        match val {
            OutgoingMessage::Notification(OutgoingNotification { method, params }) => {
                JsonRpcMessage::Notification(JsonRpcRequest::notification(method, params))
            }
            OutgoingMessage::Error(OutgoingError { id, error }) => {
                JsonRpcMessage::Response(JsonRpcResponse::error(id.to_value(), error))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OutgoingNotification {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Params envelope for `codex/event` notifications. The `_meta` field follows
/// the MCP spec's general extension mechanism.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OutgoingNotificationParams {
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<OutgoingNotificationMeta>,

    #[serde(flatten)]
    pub event: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OutgoingNotificationMeta {
    pub request_id: Option<RequestId>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<ProcessId>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OutgoingError {
    pub error: ErrorData,
    pub id: RequestId,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use anyhow::Result;
    use chaos_ipc::ProcessId;
    use chaos_ipc::openai_models::ReasoningEffort;
    use chaos_ipc::protocol::ApprovalPolicy;
    use chaos_ipc::protocol::EventMsg;
    use chaos_ipc::protocol::SandboxPolicy;
    use chaos_ipc::protocol::SessionConfiguredEvent;
    use pretty_assertions::assert_eq;
    use serde_json::json;

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
        assert!(
            obj.get("params").is_none() || obj.get("params") == Some(&serde_json::Value::Null),
            "params should be absent or null"
        );
    }

    #[tokio::test]
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
                model: "gpt-4o".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: chaos_ipc::config_types::ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
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

        let conversation_id = ProcessId::new();
        let session_configured_event = SessionConfiguredEvent {
            session_id: conversation_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-4o".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: chaos_ipc::config_types::ApprovalsReviewer::User,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
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
                "approval_policy": "headless",
                "approvals_reviewer": "user",
                "sandbox_policy": {
                    "type": "read-only"
                },
                "cwd": "/home/user/project",
                "reasoning_effort": session_configured_event.reasoning_effort,
                "history_log_id": session_configured_event.history_log_id,
                "history_entry_count": session_configured_event.history_entry_count,
            }
        });
        assert_eq!(params.unwrap(), expected_params);
        Ok(())
    }

    #[tokio::test]
    async fn test_send_event_as_notification_with_meta_and_process_id() -> Result<()> {
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);

        let process_id = ProcessId::new();
        let session_configured_event = SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-4o".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: chaos_ipc::config_types::ApprovalsReviewer::User,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
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
        assert_eq!(method, "codex/event");
        let expected_params = json!({
            "_meta": {
                "requestId": "123",
                "processId": process_id.to_string(),
            },
            "id": "1",
            "msg": {
                "type": "session_configured",
                "session_id": session_configured_event.session_id,
                "model": "gpt-4o",
                "model_provider_id": "test-provider",
                "approval_policy": "headless",
                "approvals_reviewer": "user",
                "sandbox_policy": {
                    "type": "read-only"
                },
                "cwd": "/home/user/project",
                "reasoning_effort": session_configured_event.reasoning_effort,
                "history_log_id": session_configured_event.history_log_id,
                "history_entry_count": session_configured_event.history_entry_count,
            }
        });
        assert_eq!(params.unwrap(), expected_params);
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
