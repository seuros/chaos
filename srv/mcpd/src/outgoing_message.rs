//! Session-addressed delivery; standard MCP methods use the host's gated APIs.

use std::sync::Arc;
use std::sync::Weak;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::Event;
use mcp_host::prelude::Server;
use mcp_host::prelude::Session;
use mcp_host::protocol::types::JsonRpcError;
use mcp_host::protocol::types::RequestId;
use mcp_host::server::multiplexer::ClientRequester;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tracing::warn;

mod url_relay;
pub(crate) use url_relay::UrlElicitations;
pub(crate) type ErrorData = JsonRpcError;
pub(crate) type ResponseReceiver = oneshot::Receiver<Result<Value, ErrorData>>;

/// Constructed from the originating execution context, never a process-global
/// requester. Clones retain that requester's session/task routing.
pub(crate) struct OutgoingMessageSender {
    server: Weak<Server>,
    session_id: String,
    sdk_session: Option<Session>,
    requester: Option<ClientRequester>,
    urls: Arc<UrlElicitations>,
    lifetime: Option<watch::Receiver<bool>>,
}

impl OutgoingMessageSender {
    pub(crate) fn new(
        server: Weak<Server>,
        session_id: String,
        requester: Option<ClientRequester>,
        urls: Arc<UrlElicitations>,
    ) -> Self {
        let sdk_session = server
            .upgrade()
            .and_then(|server| server.get_session(&session_id));
        Self {
            server,
            session_id,
            sdk_session,
            requester,
            urls,
            lifetime: None,
        }
    }

    pub(crate) fn with_lifetime(mut self, lifetime: watch::Receiver<bool>) -> Self {
        self.lifetime = Some(lifetime);
        self
    }

    pub(crate) fn supports_form_elicitation(&self) -> bool {
        self.active_server().is_some()
            && self
                .requester
                .as_ref()
                .is_some_and(ClientRequester::supports_elicitation)
    }

    pub(crate) fn supports_url_elicitation(&self) -> bool {
        self.active_server().is_some()
            && self
                .requester
                .as_ref()
                .is_some_and(ClientRequester::supports_url_elicitation)
    }

    pub(crate) fn request_form(
        &self,
        message: String,
        schema: Value,
        meta: Option<Map<String, Value>>,
    ) -> ResponseReceiver {
        let (mut tx, rx) = oneshot::channel();
        if self.active_server().is_none() {
            return rx;
        }
        let Some(requester) = self.requester.clone() else {
            return rx;
        };
        let lifetime = self.lifetime.clone();
        tokio::spawn(async move {
            let request = requester.request_elicitation_reply(message, schema, meta, None);
            tokio::select! {
                biased;
                _ = session_closed(lifetime) => {}
                _ = tx.closed() => {}
                result = request => {
                    let result = result
                        .map_err(|error| ErrorData::internal_error(error.to_string()))
                        .and_then(|value| serde_json::to_value(value)
                            .map_err(|error| ErrorData::internal_error(error.to_string())));
                    let _ = tx.send(result);
                }
            }
        });
        rx
    }

    /// The outer host mints its own URL id. The source URL is opaque and must
    /// not be rewritten; remember the id translation for the later completion.
    pub(crate) fn request_url(
        &self,
        process_id: ProcessId,
        server_name: String,
        source_id: String,
        message: String,
        url: String,
        meta: Option<Map<String, Value>>,
    ) -> ResponseReceiver {
        let (mut tx, rx) = oneshot::channel();
        if self.active_server().is_none() {
            return rx;
        }
        let Some(requester) = self.requester.clone() else {
            return rx;
        };
        let mut relay =
            self.urls
                .reservation(self.session_id.clone(), process_id, server_name, source_id);
        let lifetime = self.lifetime.clone();
        tokio::spawn(async move {
            let request = async {
                let result = requester
                    .request_url_elicitation_reply(
                        message,
                        |id| {
                            if relay.register(id) {
                                url
                            } else {
                                String::new()
                            }
                        },
                        meta,
                        None,
                    )
                    .await
                    .map_err(|error| ErrorData::internal_error(error.to_string()))?;
                if result.reply.result.action
                    == mcp_host::protocol::types::ElicitationAction::Accept
                {
                    relay.retain();
                }
                serde_json::to_value(result.reply)
                    .map_err(|error| ErrorData::internal_error(error.to_string()))
            };
            tokio::select! {
                biased;
                _ = session_closed(lifetime) => {}
                _ = tx.closed() => {}
                result = request => { let _ = tx.send(result); }
            }
        });
        rx
    }

    pub(crate) fn complete_url(
        &self,
        process_id: ProcessId,
        server_name: String,
        source_id: String,
    ) {
        let Some(id) = self
            .urls
            .complete(&self.session_id, process_id, server_name, source_id)
        else {
            return;
        };
        if let Some(server) = self.active_server()
            && let Err(error) = server.notify_elicitation_complete(&self.session_id, &id)
        {
            warn!(%error, "could not queue URL elicitation completion");
        }
    }

    pub(crate) async fn send_event_as_notification(
        &self,
        event: &Event,
        meta: Option<OutgoingNotificationMeta>,
    ) {
        let Some(server) = self.active_server() else {
            return;
        };
        let params = match serde_json::to_value(OutgoingNotificationParams { meta, event }) {
            Ok(params) => params,
            Err(error) => {
                warn!(%error, "could not serialize chaos/event");
                return;
            }
        };
        // No extra unbounded forwarding queue. Saturation is observable and
        // never spills a notification into another session's transport.
        if let Err(error) =
            server.send_notification_to_session(&self.session_id, "chaos/event", Some(params))
        {
            warn!(%error, "could not queue chaos/event");
        }
    }

    fn active_server(&self) -> Option<Arc<Server>> {
        if self
            .lifetime
            .as_ref()
            .is_some_and(|lifetime| *lifetime.borrow())
        {
            return None;
        }
        let original = self.sdk_session.as_ref()?;
        let server = self.server.upgrade()?;
        let active = server.get_session(&self.session_id)?;
        crate::session_state::same_session(original, &active).then_some(server)
    }
}

async fn session_closed(lifetime: Option<watch::Receiver<bool>>) {
    let Some(mut lifetime) = lifetime else {
        return std::future::pending().await;
    };
    if !*lifetime.borrow_and_update() {
        let _ = lifetime.changed().await;
    }
}

#[derive(Serialize)]
pub(crate) struct OutgoingNotificationParams<'a> {
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<OutgoingNotificationMeta>,
    #[serde(flatten)]
    pub event: &'a Event,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OutgoingNotificationMeta {
    pub request_id: Option<RequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<ProcessId>,
}

#[cfg(test)]
mod tests;
