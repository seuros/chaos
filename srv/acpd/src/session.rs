use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use acp_host::{AgentSession, SessionContext, protocol};
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::{EventMsg, Op};
use chaos_ipc::user_input::UserInput;
use chaos_kern::{Process, ProcessTable};
use protocol::schema::v1::{ContentBlock, PromptRequest, PromptResponse, StopReason};
use tokio_util::sync::CancellationToken;

use crate::{approvals, internal, invalid, projection::Projection};

pub(crate) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) async fn submit(
    process: &Process,
    op: Op,
    message: &'static str,
) -> protocol::Result<String> {
    process.submit(op).await.map_err(|_| internal(message))
}

async fn shutdown_native(table: &ProcessTable) -> protocol::Result<()> {
    let report = table.shutdown_all_processes_bounded(CLEANUP_TIMEOUT).await;
    if !report.submit_failed.is_empty()
        || !report.timed_out.is_empty()
        || !table.list_process_ids().await.is_empty()
    {
        return Err(internal("native ACP process shutdown is unconfirmed"));
    }
    Ok(())
}

pub(crate) struct OwnerGuard {
    table: Arc<ProcessTable>,
    closed: AtomicBool,
}

impl OwnerGuard {
    pub fn new(table: Arc<ProcessTable>) -> Self {
        Self {
            table,
            closed: AtomicBool::new(false),
        }
    }

    pub(crate) async fn shutdown(&self) -> protocol::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        shutdown_native(&self.table).await?;
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let table = self.table.clone();
            runtime.spawn(async move {
                if shutdown_native(&table).await.is_err() {
                    tracing::error!("ACP native process cleanup remained unconfirmed");
                }
            });
        }
    }
}

pub(crate) struct NativeSession {
    process: Arc<Process>,
    id: ProcessId,
    owner: OwnerGuard,
    active: Mutex<Option<CancellationToken>>,
    history: Mutex<Option<Vec<chaos_ipc::protocol::RolloutItem>>>,
}

impl NativeSession {
    pub fn new(process: Arc<Process>, id: ProcessId, owner: OwnerGuard) -> Self {
        Self {
            process,
            id,
            owner,
            active: Mutex::new(None),
            history: Mutex::new(None),
        }
    }

    pub fn with_history(self, history: Vec<chaos_ipc::protocol::RolloutItem>) -> Self {
        *self.history.lock().unwrap_or_else(PoisonError::into_inner) = Some(history);
        self
    }

    async fn run(
        &self,
        request: PromptRequest,
        context: SessionContext,
    ) -> protocol::Result<PromptResponse> {
        if context.is_cancelled() {
            return Ok(PromptResponse::new(StopReason::Cancelled));
        }
        let op = match crate::commands::dispatch(&self.process, request.prompt, &context).await? {
            crate::commands::Dispatch::Done => return Ok(PromptResponse::new(StopReason::EndTurn)),
            crate::commands::Dispatch::Prompt(content) => {
                self.process.user_turn(input(content)?, None).await
            }
            crate::commands::Dispatch::Native(op) => *op,
        };
        let renaming = matches!(op, Op::SetProcessName { .. });
        let mut editor_scope = None;
        let submission = if let Some(editor) = crate::files::Editor::negotiated(&context)? {
            let id = format!("acp-{}", ProcessId::new());
            editor_scope = Some(
                self.process
                    .bind_editor_files(id.clone(), Arc::new(editor))
                    .map_err(|_| internal("native ACP editor binding failed"))?,
            );
            self.process
                .submit_with_id(chaos_ipc::protocol::Submission {
                    id: id.clone(),
                    op,
                    trace: None,
                })
                .await
                .map_err(|_| {
                    internal("native ACP prompt submission failed; outcome may be unknown")
                })?;
            id
        } else {
            submit(
                &self.process,
                op,
                "native ACP prompt submission failed; outcome may be unknown",
            )
            .await?
        };
        let _editor_scope = editor_scope;
        if context.is_cancelled() {
            submit(
                &self.process,
                Op::Interrupt,
                "native ACP interruption delivery failed",
            )
            .await?;
        }
        let mut projection = Projection::default();
        loop {
            let event =
                self.process.next_event().await.map_err(|_| {
                    internal("native ACP event stream closed; outcome may be unknown")
                })?;
            if let EventMsg::SessionModeChanged(mode) = &event.msg
                && mode.session_id == self.id
            {
                crate::controls::publish(&self.process, &context).await?;
                continue;
            }
            // No other frontend consumes this process. Ignore prior/startup and
            // unrelated background events rather than attributing them to a turn.
            if event.id != submission {
                continue;
            }
            match &event.msg {
                EventMsg::ProcessNameUpdated(event) if renaming => {
                    crate::commands::message(
                        &context,
                        format!(
                            "Session renamed: {}",
                            event.process_name.as_deref().unwrap_or("")
                        ),
                    )
                    .await?;
                    return Ok(PromptResponse::new(StopReason::EndTurn));
                }
                EventMsg::ExecApprovalRequest(request) => {
                    approvals::exec(&self.process, &context, &mut projection, request).await?;
                }
                EventMsg::ApplyPatchApprovalRequest(request) => {
                    approvals::patch(&self.process, &context, &mut projection, request).await?;
                }
                EventMsg::ElicitationRequest(request) => {
                    submit(
                        &self.process,
                        Op::ResolveElicitation {
                            server_name: request.server_name.clone(),
                            request_id: request.id.clone(),
                            decision: chaos_ipc::protocol::ElicitationAction::Cancel,
                            content: None,
                            meta: None,
                        },
                        "failed to deny unsupported ACP elicitation",
                    )
                    .await?;
                }
                EventMsg::RequestUserInput(request) => {
                    submit(
                        &self.process,
                        Op::UserInputAnswer {
                            id: request.turn_id.clone(),
                            response: chaos_ipc::request_user_input::RequestUserInputResponse {
                                answers: Default::default(),
                            },
                        },
                        "failed to deny unsupported ACP user input",
                    )
                    .await?;
                }
                EventMsg::RequestPermissions(request) => {
                    submit(
                        &self.process,
                        Op::RequestPermissionsResponse {
                            id: request.call_id.clone(),
                            response: chaos_ipc::request_permissions::RequestPermissionsResponse {
                                permissions: Default::default(),
                                scope: Default::default(),
                            },
                        },
                        "failed to deny unsupported ACP permissions",
                    )
                    .await?;
                }
                EventMsg::DynamicToolCallRequest(request) => {
                    submit(
                        &self.process,
                        Op::DynamicToolResponse {
                            id: request.call_id.clone(),
                            response: chaos_ipc::dynamic_tools::DynamicToolResponse {
                                content_items: Vec::new(),
                                success: false,
                            },
                        },
                        "failed to deny unsupported ACP dynamic tool",
                    )
                    .await?;
                }
                EventMsg::Error(error) => {
                    tracing::error!(message = %error.message, "native ACP operation failed");
                    if matches!(
                        error.chaos_error_info,
                        Some(
                            chaos_ipc::protocol::ChaosErrorInfo::ProviderAuthMissing { .. }
                                | chaos_ipc::protocol::ChaosErrorInfo::Unauthorized
                        )
                    ) {
                        return Err(protocol::Error::auth_required().data(
                            "Configure the selected provider account in Chaos before starting ACP",
                        ));
                    }
                    return Err(internal(
                        "native Chaos turn failed; inspect its local diagnostics",
                    ));
                }
                EventMsg::TurnComplete(event) => {
                    context.client().capabilities()?;
                    projection.finish_text(&context, event.last_agent_message.as_deref())?;
                    return Ok(PromptResponse::new(StopReason::EndTurn));
                }
                EventMsg::TurnAborted(_) => {
                    context.client().capabilities()?;
                    projection.cancelled(&context)?;
                    return Ok(PromptResponse::new(StopReason::Cancelled));
                }
                EventMsg::ShutdownComplete => {
                    return Err(internal(
                        "native Chaos process shut down before the turn completed",
                    ));
                }
                _ => projection.event(&context, &event.msg)?,
            }
        }
    }
}

impl AgentSession for NativeSession {
    fn info(&self) -> protocol::BoxFuture<'_, protocol::Result<acp_host::SessionInfo>> {
        Box::pin(async move {
            let mut info = crate::controls::info(&self.process).await;
            info.id = Some(protocol::schema::v1::SessionId::new(self.id.to_string()));
            Ok(info)
        })
    }

    fn replay(&self, context: SessionContext) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        let history = self
            .history
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Box::pin(async move { crate::replay::history(history.unwrap_or_default(), &context).await })
    }

    fn set_config_option(
        &self,
        request: protocol::schema::v1::SetSessionConfigOptionRequest,
        _context: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<acp_host::SessionInfo>> {
        Box::pin(crate::controls::set(&self.process, request))
    }

    fn prompt(
        &self,
        request: PromptRequest,
        context: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<PromptResponse>> {
        Box::pin(async move {
            let finished = CancellationToken::new();
            *self.active.lock().unwrap_or_else(PoisonError::into_inner) = Some(finished.clone());
            let _guard = Completion(finished);
            let result = self.run(request, context).await;
            if result.is_err()
                && let Err(error) = self.owner.shutdown().await
            {
                tracing::error!(?error, "native ACP error cleanup remained unconfirmed");
            }
            result
        })
    }

    fn cancel(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async move {
            let active = self
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let Some(active) = active else { return Ok(()) };
            submit(
                &self.process,
                Op::CleanBackgroundTerminals,
                "background terminal cancellation delivery failed",
            )
            .await?;
            submit(
                &self.process,
                Op::Interrupt,
                "native turn cancellation delivery failed",
            )
            .await?;
            // The prompt is the only event consumer and signals its terminal
            // observation. The host still holds the session busy while we join children.
            active.cancelled().await;
            let ids = self.owner.table.list_process_ids().await;
            for id in ids.into_iter().filter(|id| *id != self.id) {
                if let Ok(process) = self.owner.table.get_process(id).await {
                    tokio::time::timeout(CLEANUP_TIMEOUT, process.shutdown_and_wait())
                        .await
                        .map_err(|_| internal("native descendant shutdown timed out"))?
                        .map_err(|_| internal("native descendant shutdown is unconfirmed"))?;
                    self.owner.table.remove_process(&id).await;
                }
            }
            Ok(())
        })
    }

    fn shutdown(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(self.owner.shutdown())
    }
}

struct Completion(CancellationToken);

impl Drop for Completion {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn input(content: Vec<ContentBlock>) -> protocol::Result<Vec<UserInput>> {
    content
        .into_iter()
        .map(|block| {
            let text = match block {
                ContentBlock::Text(text) => text.text,
                ContentBlock::ResourceLink(link) => {
                    // A reference is user context, not an instruction or an
                    // automatic file/network fetch outside canonical tool policy.
                    format!(
                        "Referenced resource (untrusted context): {} ({})",
                        link.uri, link.name
                    )
                }
                _ => return Err(invalid("unsupported ACP prompt content")),
            };
            Ok(UserInput::Text {
                text,
                text_elements: Vec::new(),
            })
        })
        .collect()
}
