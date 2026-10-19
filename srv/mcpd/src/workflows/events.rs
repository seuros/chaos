//! Event delivery and request-scoped interactions for a dedicated workflow process.

use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_ipc::approvals::ElicitationAction;
use chaos_ipc::approvals::ElicitationRequest;
use chaos_ipc::approvals::ElicitationRequestEvent;
use chaos_ipc::protocol::ApplyPatchApprovalRequestEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecApprovalRequestEvent;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::ReviewDecision;
use chaos_kern::Process;
use mcp_host::prelude::ToolError;
use mcp_host::protocol::types::RequestId;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tokio::task::JoinSet;

use crate::elicitation::ApprovalElicitationAction;
use crate::elicitation::ApprovalElicitationResponse;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingNotificationMeta;
use crate::outgoing_message::ResponseReceiver;

const MAX_PENDING_INTERACTIONS: usize = 16;

/// Runs alongside the external root. No native turn is expected on this fresh
/// process: even a normal turn completion means the root must be stopped.
///
/// Dropping this future drops the JoinSet, aborting every interaction worker.
/// Their receiver drops in turn cancel the outgoing SDK requests.
pub(super) async fn pump(
    process_id: ProcessId,
    process: Arc<Process>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
) -> Result<(), ToolError> {
    let relay = Relay {
        process_id,
        process,
        outgoing,
        request_id,
    };
    let mut workers = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            result = workers.join_next(), if !workers.is_empty() => {
                if let Some(result) = result {
                    result.map_err(|error| ToolError::Internal(format!(
                        "workflow interaction worker stopped: {error}"
                    )))??;
                }
            }
            event = relay.process.next_event() => {
                let event = event.map_err(|error| ToolError::Execution(format!(
                    "workflow process event channel stopped: {error}"
                )))?;
                relay.outgoing.send_event_as_notification(
                    &event,
                    Some(OutgoingNotificationMeta {
                        request_id: Some(relay.request_id.clone()),
                        process_id: Some(process_id),
                    }),
                ).await;
                relay.dispatch(event, &mut workers).await?;
            }
        }
    }
}

#[derive(Clone)]
struct Relay {
    process_id: ProcessId,
    process: Arc<Process>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
}

enum Interaction {
    Exec(String, Box<ExecApprovalRequestEvent>),
    Patch(String, ApplyPatchApprovalRequestEvent),
    Elicitation(ElicitationRequestEvent),
}

impl Relay {
    async fn dispatch(
        &self,
        event: Event,
        workers: &mut JoinSet<Result<(), ToolError>>,
    ) -> Result<(), ToolError> {
        let interaction = match event.msg {
            EventMsg::ExecApprovalRequest(request) => {
                Interaction::Exec(event.id, Box::new(request))
            }
            EventMsg::ApplyPatchApprovalRequest(request) => Interaction::Patch(event.id, request),
            EventMsg::ElicitationRequest(request) => Interaction::Elicitation(request),
            EventMsg::ElicitationComplete(event) => {
                self.outgoing.complete_url(
                    self.process_id,
                    event.server_name,
                    event.elicitation_id,
                );
                return Ok(());
            }
            EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) | EventMsg::ShutdownComplete => {
                return Err(ToolError::Execution(
                    "dedicated workflow process stopped before the external root completed".into(),
                ));
            }
            EventMsg::Error(event) => return Err(ToolError::Execution(event.message)),
            _ => return Ok(()), // Every event was already forwarded above.
        };
        if workers.len() >= MAX_PENDING_INTERACTIONS {
            // Do not leave the newest callback unanswered or open an unbounded
            // queue. Failing the pump also aborts the root and existing workers.
            self.submit(interaction.resolve(None)).await?;
            return Err(ToolError::Execution(
                "workflow interaction limit reached".into(),
            ));
        }
        let relay = self.clone();
        workers.spawn(async move { relay.reply(interaction).await });
        Ok(())
    }

    async fn reply(&self, interaction: Interaction) -> Result<(), ToolError> {
        let Some(receiver) = self.receiver(&interaction) else {
            return self.submit(interaction.resolve(None)).await;
        };
        let (response, failure) = match decode_reply(receiver).await {
            Ok(response) => (response, None),
            Err(error) => (decline(), Some(error)),
        };
        self.submit(interaction.resolve(Some(response))).await?;
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn submit(&self, op: Op) -> Result<(), ToolError> {
        self.process.submit(op).await.map(|_| ()).map_err(|error| {
            ToolError::Execution(format!("could not resolve workflow interaction: {error}"))
        })
    }

    fn receiver(&self, interaction: &Interaction) -> Option<ResponseReceiver> {
        match interaction {
            Interaction::Exec(event_id, request) => {
                let command = shlex::try_join(request.command.iter().map(String::as_str))
                    .unwrap_or_else(|_| request.command.join(" "));
                self.approval_form(
                    event_id,
                    "exec-approval",
                    format!(
                        "Allow Chaos to run `{command}` in `{}`?",
                        request.cwd.display()
                    ),
                    request,
                )
            }
            Interaction::Patch(event_id, request) => {
                let question = "Allow Chaos to apply proposed code changes?";
                let message = match &request.reason {
                    Some(reason) => format!("{reason}\n{question}"),
                    None => question.to_string(),
                };
                self.approval_form(event_id, "patch-approval", message, request)
            }
            Interaction::Elicitation(request) => self.elicitation(request),
        }
    }

    fn approval_form(
        &self,
        event_id: &str,
        kind: &str,
        message: String,
        details: &impl Serialize,
    ) -> Option<ResponseReceiver> {
        if !self.outgoing.supports_form_elicitation() {
            return None;
        }
        let meta = match approval_meta(self.process_id, &self.request_id, event_id, kind, details) {
            Ok(meta) => meta,
            Err(error) => {
                tracing::warn!(%error, "could not encode workflow approval metadata; denying");
                return None;
            }
        };
        Some(self.outgoing.request_form(
            message,
            json!({"type": "object", "properties": {}}),
            Some(meta),
        ))
    }

    fn elicitation(&self, request: &ElicitationRequestEvent) -> Option<ResponseReceiver> {
        match &request.request {
            ElicitationRequest::Form {
                meta,
                message,
                requested_schema,
            } if self.outgoing.supports_form_elicitation() => Some(self.outgoing.request_form(
                message.clone(),
                requested_schema.clone(),
                forwarded_meta(meta.clone()).ok()?,
            )),
            ElicitationRequest::Url {
                meta,
                message,
                url,
                elicitation_id,
            } if self.outgoing.supports_url_elicitation() => Some(self.outgoing.request_url(
                self.process_id,
                request.server_name.clone(),
                elicitation_id.clone(),
                message.clone(),
                url.clone(),
                forwarded_meta(meta.clone()).ok()?,
            )),
            _ => None,
        }
    }
}

impl Interaction {
    fn resolve(&self, response: Option<ApprovalElicitationResponse>) -> Op {
        match self {
            Self::Exec(event_id, request) => Op::ExecApproval {
                id: request.effective_approval_id(),
                turn_id: Some(if request.turn_id.is_empty() {
                    event_id.clone()
                } else {
                    request.turn_id.clone()
                }),
                decision: response
                    .map(|response| response.review_decision())
                    .unwrap_or(ReviewDecision::Denied),
            },
            Self::Patch(_, request) => Op::PatchApproval {
                id: request.call_id.clone(),
                decision: response
                    .map(|response| response.review_decision())
                    .unwrap_or(ReviewDecision::Denied),
            },
            Self::Elicitation(request) => {
                let (decision, content, meta) = match response {
                    Some(response) => (response.core_action(), response.content, response.meta),
                    None => (ElicitationAction::Cancel, None, None),
                };
                Op::ResolveElicitation {
                    server_name: request.server_name.clone(),
                    request_id: request.id.clone(),
                    decision,
                    content,
                    meta,
                }
            }
        }
    }
}

/// Only native approval events enter here, never forwarded server/client meta.
/// Keep all approval details, including policy context, under Chaos-owned keys.
fn approval_meta(
    process_id: ProcessId,
    request_id: &RequestId,
    event_id: &str,
    kind: &str,
    details: &impl Serialize,
) -> Result<Map<String, Value>, serde_json::Error> {
    let details: Map<String, Value> = serde_json::from_value(serde_json::to_value(details)?)?;
    let mut meta: Map<String, Value> = details
        .into_iter()
        .map(|(key, value)| (format!("chaos_{key}"), value))
        .collect();
    meta.insert("chaos_elicitation".into(), json!(kind));
    meta.insert("chaos_process_id".into(), json!(process_id));
    meta.insert("chaos_request_id".into(), json!(request_id));
    meta.insert("chaos_event_id".into(), json!(event_id));
    Ok(meta)
}

fn forwarded_meta(meta: Option<Value>) -> Result<Option<Map<String, Value>>, &'static str> {
    match meta {
        None => Ok(None),
        Some(Value::Object(meta)) => Ok(Some(meta)),
        Some(_) => {
            tracing::warn!("invalid forwarded workflow elicitation metadata; cancelling");
            Err("_meta must be an object")
        }
    }
}

fn decline() -> ApprovalElicitationResponse {
    ApprovalElicitationResponse {
        action: ApprovalElicitationAction::Decline,
        content: None,
        meta: None,
    }
}

async fn decode_reply(
    receiver: ResponseReceiver,
) -> Result<ApprovalElicitationResponse, ToolError> {
    match receiver.await {
        Ok(Ok(value)) => Ok(serde_json::from_value(value).unwrap_or_else(|error| {
            tracing::warn!(%error, "invalid workflow elicitation reply; declining");
            decline()
        })),
        Ok(Err(error)) => {
            tracing::warn!(?error, "workflow elicitation failed; declining");
            Ok(decline())
        }
        Err(error) => Err(ToolError::Internal(format!(
            "workflow interaction reply channel closed: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[test]
    fn approval_metadata_is_native_and_namespaced() -> Result<(), serde_json::Error> {
        let pid = ProcessId::new();
        let request_id = RequestId::Number(7.into());
        let meta = approval_meta(
            pid,
            &request_id,
            "event",
            "exec-approval",
            &json!({
                "call_id": "call",
                "approval_id": "callback",
                "turn_id": "turn",
                "command": ["echo", "hello"],
                "reason": "sandbox retry"
            }),
        )?;
        assert!(meta.keys().all(|key| key.starts_with("chaos_")));
        assert_eq!(meta["chaos_process_id"], json!(pid));
        assert_eq!(meta["chaos_request_id"], json!(request_id));
        assert_eq!(meta["chaos_event_id"], "event");
        assert_eq!(meta["chaos_approval_id"], "callback");
        assert_eq!(meta["chaos_turn_id"], "turn");
        assert_eq!(meta["chaos_command"], json!(["echo", "hello"]));
        Ok(())
    }

    #[test]
    fn forwarded_metadata_requires_an_object() {
        assert_eq!(forwarded_meta(None), Ok(None));
        let meta = json!({"source": "inner-server", "opaque": [1, 2]});
        assert_eq!(
            forwarded_meta(Some(meta.clone())).map(|meta| meta.map(Value::Object)),
            Ok(Some(meta))
        );
        for invalid in [Value::Null, json!(1), json!("meta"), json!([])] {
            assert!(forwarded_meta(Some(invalid)).is_err());
        }
    }

    #[test]
    fn native_resolution_uses_the_callback_and_native_turn() -> Result<(), serde_json::Error> {
        let request = serde_json::from_value(json!({
            "call_id": "command-item",
            "approval_id": "subcommand-callback",
            "turn_id": "native-turn",
            "command": ["echo", "hello"],
            "cwd": "/tmp",
            "parsed_cmd": []
        }))?;
        let interaction = Interaction::Exec("event".into(), Box::new(request));
        assert!(matches!(
            interaction.resolve(None),
            Op::ExecApproval { id, turn_id, decision: ReviewDecision::Denied }
                if id == "subcommand-callback" && turn_id.as_deref() == Some("native-turn")
        ));
        let patch = Interaction::Patch(
            "event".into(),
            ApplyPatchApprovalRequestEvent {
                call_id: "patch-call".into(),
                turn_id: "native-turn".into(),
                changes: Default::default(),
                reason: None,
                grant_root: None,
            },
        );
        assert!(matches!(
            patch.resolve(None),
            Op::PatchApproval { id, decision: ReviewDecision::Denied } if id == "patch-call"
        ));
        Ok(())
    }

    #[test]
    fn forwarded_resolution_cancels_unsupported_and_preserves_answers() {
        let interaction = Interaction::Elicitation(ElicitationRequestEvent {
            turn_id: None,
            server_name: "inner".into(),
            id: chaos_ipc::mcp::RequestId::Integer(7),
            request: ElicitationRequest::Form {
                meta: None,
                message: "confirm".into(),
                requested_schema: json!({"type": "object", "properties": {}}),
            },
        });
        assert!(matches!(
            interaction.resolve(None),
            Op::ResolveElicitation {
                server_name, request_id: chaos_ipc::mcp::RequestId::Integer(7),
                decision: ElicitationAction::Cancel, content: None, meta: None,
            } if server_name == "inner"
        ));
        let content = Some(json!({"confirmed": true}));
        let meta = Some(json!({"source": "client"}));
        let response = ApprovalElicitationResponse {
            action: ApprovalElicitationAction::Accept,
            content: content.clone(),
            meta: meta.clone(),
        };
        match interaction.resolve(Some(response)) {
            Op::ResolveElicitation {
                decision,
                content: actual_content,
                meta: actual_meta,
                ..
            } => {
                assert_eq!(decision, ElicitationAction::Accept);
                assert_eq!(actual_content, content);
                assert_eq!(actual_meta, meta);
            }
            _ => panic!("expected elicitation resolution"),
        }
    }

    #[tokio::test]
    async fn replies_preserve_content_and_meta_but_errors_deny() -> Result<(), ToolError> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Ok(json!({
            "action": "accept",
            "content": {"confirmed": true},
            "_meta": {"opaque": "client context"}
        })));
        let reply = decode_reply(rx).await?;
        assert_eq!(reply.review_decision(), ReviewDecision::Approved);
        assert_eq!(reply.content, Some(json!({"confirmed": true})));
        assert_eq!(reply.meta, Some(json!({"opaque": "client context"})));
        for value in [json!({"action": "decline"}), json!({"action": "invalid"})] {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Ok(value));
            assert_eq!(
                decode_reply(rx).await?.review_decision(),
                ReviewDecision::Denied
            );
        }
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(crate::outgoing_message::ErrorData::internal_error(
            "failed",
        )));
        assert_eq!(
            decode_reply(rx).await?.review_decision(),
            ReviewDecision::Denied
        );
        Ok(())
    }

    #[tokio::test]
    async fn closed_reply_channel_is_fatal() {
        let (tx, rx) = oneshot::channel();
        drop(tx);
        assert!(decode_reply(rx).await.is_err());
    }

    #[tokio::test]
    async fn dropping_workers_cancels_all_pending_receivers() {
        let mut workers = JoinSet::new();
        let mut senders = Vec::new();
        for _ in 0..MAX_PENDING_INTERACTIONS {
            let (tx, rx) = oneshot::channel();
            senders.push(tx);
            workers.spawn(decode_reply(rx));
        }
        assert_eq!(workers.len(), MAX_PENDING_INTERACTIONS);
        drop(workers);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            for mut tx in senders {
                tx.closed().await;
            }
        })
        .await
        .expect("dropping the pump must cancel pending SDK request receivers");
    }
}
