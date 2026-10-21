#![cfg(test)]

use acp_host::*;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use acp_guest::{AcpConnection, ConnectionOptions};
use protocol::schema::v1::{
    CancelNotification, ContentBlock, ContentChunk, NewSessionRequest, PermissionOption,
    PermissionOptionKind, PromptRequest, PromptResponse, RequestPermissionOutcome, SessionId,
    SessionNotification, SessionUpdate, StopReason, TextContent,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[path = "support/closing.rs"]
mod closing;
#[path = "support/controls.rs"]
mod controls;
#[path = "support/elicitation.rs"]
mod elicitation;
#[path = "support/faults.rs"]
mod faults;
#[path = "support/files.rs"]
mod files;
#[path = "support/permissions.rs"]
mod permissions;
#[path = "support/saved.rs"]
mod saved;

#[derive(Default)]
struct Observations {
    shutdowns: AtomicUsize,
    cancels: AtomicUsize,
    context: Mutex<Option<SessionContext>>,
    slow_cancel: AtomicBool,
    cancel_release: tokio::sync::Notify,
    fail_shutdown: AtomicBool,
    wait_metadata: AtomicBool,
    metadata_started: tokio::sync::Notify,
}

struct Backend(Arc<Observations>);

struct NativeSession {
    observations: Arc<Observations>,
    prompts: AtomicUsize,
}

impl AgentBackend for Backend {
    fn new_session(
        &self,
        _request: NewSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async {
            Ok(Arc::new(NativeSession {
                observations: self.0.clone(),
                prompts: AtomicUsize::new(0),
            }) as Arc<dyn AgentSession>)
        })
    }
}

fn message(text: impl Into<String>) -> SessionUpdate {
    SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
        text,
    ))))
}

impl AgentSession for NativeSession {
    fn info(&self) -> protocol::BoxFuture<'_, protocol::Result<SessionInfo>> {
        Box::pin(async move {
            if self.observations.wait_metadata.load(Ordering::Acquire) {
                self.observations.metadata_started.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(SessionInfo::default())
        })
    }

    fn prompt(
        &self,
        request: PromptRequest,
        context: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<PromptResponse>> {
        Box::pin(async move {
            let count = self.prompts.fetch_add(1, Ordering::SeqCst) + 1;
            *self.observations.context.lock().unwrap() = Some(context.clone());
            let text = request
                .prompt
                .iter()
                .find_map(|content| match content {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .unwrap_or("resource");
            if text == "wait" {
                context.client().update(message("waiting"))?;
                context.cancelled().await;
                return Ok(PromptResponse::new(StopReason::Cancelled));
            }
            if text == "flood" {
                for _ in 0..1024 {
                    context.client().update(message("update"))?;
                }
            }
            if text == "oversized" {
                context.client().update(message("x".repeat(4096)))?;
            }
            if text == "editor-read" {
                let result = context
                    .client()
                    .read_text_file(std::path::Path::new("/workspace/buffer.rs"))
                    .await;
                if context.is_cancelled() {
                    return Ok(PromptResponse::new(StopReason::Cancelled));
                }
                context.client().update(message(result?))?;
            }
            if text == "editor-write" {
                context
                    .client()
                    .write_text_file(
                        std::path::Path::new("/workspace/buffer.rs"),
                        "replacement\n".into(),
                    )
                    .await?;
            }
            if text == "elicit" {
                elicitation::ask(&context).await?;
                if context.is_cancelled() {
                    return Ok(PromptResponse::new(StopReason::Cancelled));
                }
            }
            if text == "permission" {
                let tool = serde_json::from_value(serde_json::json!({
                    "toolCallId":"tool-1","title":"Read fixture","status":"pending"
                }))
                .unwrap();
                let result = context
                    .client()
                    .request_permission(
                        tool,
                        vec![
                            PermissionOption::new(
                                "allow",
                                "Allow once",
                                PermissionOptionKind::AllowOnce,
                            ),
                            PermissionOption::new(
                                "deny",
                                "Reject once",
                                PermissionOptionKind::RejectOnce,
                            ),
                        ],
                    )
                    .await?;
                if context.is_cancelled() {
                    return Ok(PromptResponse::new(StopReason::Cancelled));
                }
                if matches!(result.outcome, RequestPermissionOutcome::Selected(selected)
                    if selected.option_id.0.as_ref() == "allow")
                {
                    context.client().update(message("allowed"))?;
                } else {
                    context.client().update(message("denied"))?;
                }
            }
            context
                .client()
                .update(message(format!("{count}:{text}")))?;
            Ok(PromptResponse::new(StopReason::EndTurn))
        })
    }

    fn cancel(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async {
            self.observations.cancels.fetch_add(1, Ordering::SeqCst);
            if self.observations.slow_cancel.load(Ordering::SeqCst) {
                self.observations.cancel_release.notified().await;
            }
            Ok(())
        })
    }

    fn shutdown(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async {
            self.observations.shutdowns.fetch_add(1, Ordering::SeqCst);
            if self.observations.fail_shutdown.load(Ordering::SeqCst) {
                return Err(protocol::Error::internal_error().data("fixture shutdown unknown"));
            }
            Ok(())
        })
    }
}

type Running = (
    AcpConnection,
    mpsc::Receiver<SessionNotification>,
    JoinHandle<Result<(), HostError>>,
    Arc<Observations>,
);

async fn start(limits: ConnectionLimits) -> Running {
    let (a, task, observations) = serve(limits, 64 * 1024);
    let (reader, writer) = tokio::io::split(a);
    let (connection, updates) = AcpConnection::connect(
        reader,
        writer,
        ConnectionOptions {
            request_timeout: Duration::from_secs(3),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (connection, updates, task, observations)
}

fn serve(
    limits: ConnectionLimits,
    capacity: usize,
) -> (
    tokio::io::DuplexStream,
    JoinHandle<Result<(), HostError>>,
    Arc<Observations>,
) {
    let observations = Arc::new(Observations::default());
    let server = AgentServer::builder(Backend(observations.clone()))
        .limits(limits)
        .build()
        .unwrap();
    let (a, b) = tokio::io::duplex(capacity);
    let (reader, writer) = tokio::io::split(b);
    let task = tokio::spawn(async move { server.serve(reader, writer).await });
    (a, task, observations)
}

async fn session(connection: &AcpConnection) -> SessionId {
    connection
        .client()
        .request(NewSessionRequest::new(std::path::PathBuf::from(
            "/workspace",
        )))
        .await
        .unwrap()
        .session_id
}

fn prompt(id: &SessionId, text: &str) -> PromptRequest {
    PromptRequest::new(id.clone(), vec![ContentBlock::Text(TextContent::new(text))])
}

#[tokio::test]
async fn negotiated_capabilities_are_conservative() {
    let (connection, _updates, task, _) = start(ConnectionLimits::default()).await;
    let initialized = serde_json::to_value(connection.initialized()).unwrap();
    assert_eq!(initialized["protocolVersion"], 1);
    assert_ne!(initialized["agentCapabilities"]["loadSession"], true);
    assert_ne!(
        initialized["agentCapabilities"]["promptCapabilities"]["image"],
        true
    );
    assert_eq!(initialized["authMethods"], serde_json::json!([]));
    connection.disconnect().await.unwrap();
    let _ = task.await.unwrap();
}

#[tokio::test]
async fn continued_prompts_and_sessions_are_isolated() {
    let (connection, mut updates, task, observations) = start(ConnectionLimits::default()).await;
    let first = session(&connection).await;
    let second = session(&connection).await;
    assert_ne!(first, second);
    for (id, input, expected) in [
        (&first, "one", "1:one"),
        (&first, "two", "2:two"),
        (&second, "other", "1:other"),
    ] {
        let response = connection
            .client()
            .request(prompt(id, input))
            .await
            .unwrap();
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        let update = updates.recv().await.unwrap();
        assert_eq!(&update.session_id, id);
        assert_eq!(
            serde_json::to_value(update).unwrap()["update"]["content"]["text"],
            expected
        );
    }
    let stale = observations.context.lock().unwrap().clone().unwrap();
    assert!(stale.client().update(message("late")).is_err());
    connection.disconnect().await.unwrap();
    let _ = task.await.unwrap();
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn busy_rejection_keeps_cancellation_and_other_sessions_responsive() {
    let (connection, mut updates, task, observations) = start(ConnectionLimits::default()).await;
    let first = session(&connection).await;
    let second = session(&connection).await;
    let client = connection.client();
    let pending = {
        let first = first.clone();
        let client = client.clone();
        tokio::spawn(async move { client.request(prompt(&first, "wait")).await })
    };
    updates.recv().await.unwrap();
    assert!(client.request(prompt(&first, "overlap")).await.is_err());
    assert_eq!(
        client
            .request(prompt(&second, "independent"))
            .await
            .unwrap()
            .stop_reason,
        StopReason::EndTurn
    );
    updates.recv().await.unwrap();
    client
        .notify(CancelNotification::new(first.clone()))
        .unwrap();
    assert_eq!(
        pending.await.unwrap().unwrap().stop_reason,
        StopReason::Cancelled
    );
    assert_eq!(observations.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(
        client
            .request(prompt(&first, "after"))
            .await
            .unwrap()
            .stop_reason,
        StopReason::EndTurn
    );
    updates.recv().await.unwrap();
    connection.disconnect().await.unwrap();
    let _ = task.await.unwrap();
}

#[tokio::test]
async fn unknown_sessions_cannot_control_another_connection() {
    let (owner, _a_updates, a_task, _) = start(ConnectionLimits::default()).await;
    let id = session(&owner).await;
    let (other, _b_updates, b_task, _) = start(ConnectionLimits::default()).await;
    assert!(
        other
            .client()
            .request(prompt(&id, "foreign"))
            .await
            .is_err()
    );
    owner.disconnect().await.unwrap();
    other.disconnect().await.unwrap();
    let _ = a_task.await.unwrap();
    let _ = b_task.await.unwrap();
}

#[tokio::test]
async fn session_limit_is_checked_before_native_creation() {
    let (connection, _updates, task, observations) = start(ConnectionLimits {
        max_sessions: 1,
        ..Default::default()
    })
    .await;
    session(&connection).await;
    assert!(
        connection
            .client()
            .request(NewSessionRequest::new(std::path::PathBuf::from(
                "/workspace"
            )),)
            .await
            .is_err()
    );
    connection.disconnect().await.unwrap();
    let _ = task.await.unwrap();
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 1);
}
