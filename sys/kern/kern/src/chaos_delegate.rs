use std::collections::HashMap;
use std::sync::Arc;

use async_channel::Receiver;
use async_channel::Sender;
use chaos_epoll::OrCancelExt;
use chaos_ipc::protocol::ApplyPatchApprovalRequestEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecApprovalRequestEvent;
use chaos_ipc::protocol::McpInvocation;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::RequestUserInputEvent;

use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;
use chaos_ipc::protocol::Submission;
use chaos_ipc::request_permissions::PermissionGrantScope;
use chaos_ipc::request_permissions::RequestPermissionsArgs;
use chaos_ipc::request_permissions::RequestPermissionsEvent;
use chaos_ipc::request_permissions::RequestPermissionsResponse;
use chaos_ipc::request_user_input::RequestUserInputArgs;
use chaos_ipc::request_user_input::RequestUserInputResponse;
use chaos_ipc::user_input::UserInput;

use serde_json::Value;
use std::time::Duration;
use tokio::sync::Mutex;

use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::AuthManager;
use crate::chaos::Chaos;
use crate::chaos::ChaosSpawnArgs;
use crate::chaos::ChaosSpawnOk;
use crate::chaos::SUBMISSION_CHANNEL_CAPACITY;
use crate::chaos::Session;
use crate::chaos::TurnContext;
use crate::config::Config;
use crate::error::ChaosErr;

use crate::models_manager::manager::ModelsManager;
use chaos_ipc::protocol::InitialHistory;

#[cfg(test)]
use crate::chaos::completed_session_loop_termination;

/// Start an interactive sub-Chaos thread and return IO channels.
///
/// The returned `events_rx` yields non-approval events emitted by the sub-agent.
/// Approval requests are handled via `parent_session` and are not surfaced.
/// The returned `ops_tx` allows the caller to submit additional `Op`s to the sub-agent.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_chaos_process_interactive(
    config: Config,
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    parent_session: Arc<Session>,
    parent_ctx: Arc<TurnContext>,
    cancel_token: CancellationToken,
    subagent_source: SubAgentSource,
    initial_history: Option<InitialHistory>,
) -> Result<Chaos, ChaosErr> {
    let (tx_sub, rx_sub) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
    let (tx_ops, rx_ops) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);

    let ChaosSpawnOk { chaos, .. } = Chaos::spawn(ChaosSpawnArgs {
        config,
        auth_manager,
        models_manager,
        skills_manager: Arc::clone(&parent_session.services.skills_manager),
        mcp_manager: Arc::clone(&parent_session.services.mcp_manager),
        file_watcher: Arc::clone(&parent_session.services.file_watcher),
        conversation_history: initial_history.unwrap_or(InitialHistory::New),
        session_source: SessionSource::SubAgent(subagent_source),
        agent_control: parent_session.services.agent_control.clone(),
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        metrics_service_name: None,
        inherited_shell_snapshot: None,
        parent_trace: None,
    })
    .await?;
    let chaos = Arc::new(chaos);

    // Use a child token so parent cancel cascades but we can scope it to this task
    let cancel_token_events = cancel_token.child_token();
    let cancel_token_ops = cancel_token.child_token();

    // Forward events from the sub-agent to the consumer, filtering approvals and
    // routing them to the parent session for decisions.
    let parent_session_clone = Arc::clone(&parent_session);
    let parent_ctx_clone = Arc::clone(&parent_ctx);
    let codex_for_events = Arc::clone(&chaos);
    let pending_mcp_invocations = Arc::new(Mutex::new(HashMap::<String, McpInvocation>::new()));
    tokio::spawn(async move {
        forward_events(
            codex_for_events,
            tx_sub,
            parent_session_clone,
            parent_ctx_clone,
            pending_mcp_invocations,
            cancel_token_events,
        )
        .await;
    });

    // Forward ops from the caller to the sub-agent.
    let codex_for_ops = Arc::clone(&chaos);
    tokio::spawn(async move {
        forward_ops(codex_for_ops, rx_ops, cancel_token_ops).await;
    });

    Ok(Chaos {
        tx_sub: tx_ops,
        rx_event: rx_sub,
        agent_status: chaos.agent_status.clone(),
        session: Arc::clone(&chaos.session),
        session_loop_termination: chaos.session_loop_termination.clone(),
    })
}

/// Convenience wrapper for one-time use with an initial prompt.
///
/// Internally calls the interactive variant, then immediately submits the provided input.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_chaos_process_one_shot(
    config: Config,
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    input: Vec<UserInput>,
    parent_session: Arc<Session>,
    parent_ctx: Arc<TurnContext>,
    cancel_token: CancellationToken,
    subagent_source: SubAgentSource,
    final_output_json_schema: Option<Value>,
    initial_history: Option<InitialHistory>,
) -> Result<Chaos, ChaosErr> {
    // Use a child token so we can stop the delegate after completion without
    // requiring the caller to cancel the parent token.
    let child_cancel = cancel_token.child_token();
    let io = run_chaos_process_interactive(
        config,
        auth_manager,
        models_manager,
        parent_session,
        parent_ctx,
        child_cancel.clone(),
        subagent_source,
        initial_history,
    )
    .await?;

    // Send the initial input to kick off the one-shot turn.
    io.submit(Op::UserInput {
        items: input,
        final_output_json_schema,
    })
    .await?;

    // Bridge events so we can observe completion and shut down automatically.
    let (tx_bridge, rx_bridge) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
    let ops_tx = io.tx_sub.clone();
    let agent_status = io.agent_status.clone();
    let session = Arc::clone(&io.session);
    let session_loop_termination = io.session_loop_termination.clone();
    let io_for_bridge = io;
    tokio::spawn(async move {
        while let Ok(event) = io_for_bridge.next_event().await {
            let should_shutdown = matches!(
                event.msg,
                EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_)
            );
            let _ = tx_bridge.send(event).await;
            if should_shutdown {
                let _ = ops_tx
                    .send(Submission {
                        id: "shutdown".to_string(),
                        op: Op::Shutdown {},
                        trace: None,
                    })
                    .await;
                child_cancel.cancel();
                break;
            }
        }
    });

    // For one-shot usage, return a closed `tx_sub` so callers cannot submit
    // additional ops after the initial request. Create a channel and drop the
    // receiver to close it immediately.
    let (tx_closed, rx_closed) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
    drop(rx_closed);

    Ok(Chaos {
        rx_event: rx_bridge,
        tx_sub: tx_closed,
        agent_status,
        session,
        session_loop_termination,
    })
}

async fn forward_events(
    chaos: Arc<Chaos>,
    tx_sub: Sender<Event>,
    parent_session: Arc<Session>,
    parent_ctx: Arc<TurnContext>,
    pending_mcp_invocations: Arc<Mutex<HashMap<String, McpInvocation>>>,
    cancel_token: CancellationToken,
) {
    let cancelled = cancel_token.cancelled();
    tokio::pin!(cancelled);

    loop {
        tokio::select! {
            _ = &mut cancelled => {
                shutdown_delegate(&chaos).await;
                break;
            }
            event = chaos.next_event() => {
                let event = match event {
                    Ok(event) => event,
                    Err(_) => break,
                };
                match event {
                    Event {
                        id: _,
                        msg: EventMsg::TokenCount(_),
                    } => {}
                    Event {
                        id: _,
                        msg: EventMsg::SessionConfigured(_),
                    } => {}
                    Event {
                        id: _,
                        msg: EventMsg::ProcessNameUpdated(_),
                    } => {}
                    Event {
                        id,
                        msg: EventMsg::ExecApprovalRequest(event),
                    } => {
                        // Initiate approval via parent session; do not surface to consumer.
                        handle_exec_approval(
                            &chaos,
                            id,
                            &parent_session,
                            &parent_ctx,
                            event,
                            &cancel_token,
                        )
                        .await;
                    }
                    Event {
                        id,
                        msg: EventMsg::ApplyPatchApprovalRequest(event),
                    } => {
                        handle_patch_approval(
                            &chaos,
                            id,
                            &parent_session,
                            &parent_ctx,
                            event,
                            &cancel_token,
                        )
                        .await;
                    }
                    Event {
                        msg: EventMsg::RequestPermissions(event),
                        ..
                    } => {
                        handle_request_permissions(
                            &chaos,
                            &parent_session,
                            &parent_ctx,
                            event,
                            &cancel_token,
                        )
                        .await;
                    }
                    Event {
                        id,
                        msg: EventMsg::RequestUserInput(event),
                    } => {
                        handle_request_user_input(
                            &chaos,
                            id,
                            &parent_session,
                            &parent_ctx,
                            &pending_mcp_invocations,
                            event,
                            &cancel_token,
                        )
                        .await;
                    }
                    Event {
                        id,
                        msg: EventMsg::McpToolCallBegin(event),
                    } => {
                        pending_mcp_invocations
                            .lock()
                            .await
                            .insert(event.call_id.clone(), event.invocation.clone());
                        if !forward_event_or_shutdown(
                            &chaos,
                            &tx_sub,
                            &cancel_token,
                            Event {
                                id,
                                msg: EventMsg::McpToolCallBegin(event),
                            },
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Event {
                        id,
                        msg: EventMsg::McpToolCallEnd(event),
                    } => {
                        pending_mcp_invocations.lock().await.remove(&event.call_id);
                        if !forward_event_or_shutdown(
                            &chaos,
                            &tx_sub,
                            &cancel_token,
                            Event {
                                id,
                                msg: EventMsg::McpToolCallEnd(event),
                            },
                        )
                        .await
                        {
                            break;
                        }
                    }
                    other => {
                        if !forward_event_or_shutdown(&chaos, &tx_sub, &cancel_token, other).await
                        {
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// Ask the delegate to stop and drain its events so background sends do not hit a closed channel.
async fn shutdown_delegate(chaos: &Chaos) {
    let _ = chaos.submit(Op::Interrupt).await;
    let _ = chaos.submit(Op::Shutdown {}).await;

    let _ = timeout(Duration::from_millis(500), async {
        while let Ok(event) = chaos.next_event().await {
            if matches!(
                event.msg,
                EventMsg::TurnAborted(_) | EventMsg::TurnComplete(_)
            ) {
                break;
            }
        }
    })
    .await;
}

async fn forward_event_or_shutdown(
    chaos: &Chaos,
    tx_sub: &Sender<Event>,
    cancel_token: &CancellationToken,
    event: Event,
) -> bool {
    match tx_sub.send(event).or_cancel(cancel_token).await {
        Ok(Ok(())) => true,
        _ => {
            shutdown_delegate(chaos).await;
            false
        }
    }
}

/// Forward ops from a caller to a sub-agent, respecting cancellation.
async fn forward_ops(
    chaos: Arc<Chaos>,
    rx_ops: Receiver<Submission>,
    cancel_token_ops: CancellationToken,
) {
    loop {
        let submission = match rx_ops.recv().or_cancel(&cancel_token_ops).await {
            Ok(Ok(submission)) => submission,
            Ok(Err(_)) | Err(_) => break,
        };
        let _ = chaos.submit_with_id(submission).await;
    }
}

/// Handle an ExecApprovalRequest by consulting the parent session and replying.
async fn handle_exec_approval(
    chaos: &Chaos,
    turn_id: String,
    parent_session: &Arc<Session>,
    parent_ctx: &Arc<TurnContext>,
    event: ExecApprovalRequestEvent,
    cancel_token: &CancellationToken,
) {
    let approval_id_for_op = event.effective_approval_id();
    let ExecApprovalRequestEvent {
        call_id,
        approval_id,
        command,
        cwd,
        reason,
        network_approval_context,
        proposed_execpolicy_amendment,
        additional_permissions,
        skill_metadata,
        available_decisions,
        ..
    } = event;
    let decision = await_approval_with_cancel(
        parent_session.request_command_approval(
            parent_ctx,
            call_id,
            approval_id,
            command,
            cwd,
            reason,
            network_approval_context,
            proposed_execpolicy_amendment,
            additional_permissions,
            skill_metadata,
            available_decisions,
        ),
        parent_session,
        &approval_id_for_op,
        cancel_token,
        /*review_cancel_token*/ None,
    )
    .await;

    let _ = chaos
        .submit(Op::ExecApproval {
            id: approval_id_for_op,
            turn_id: Some(turn_id),
            decision,
        })
        .await;
}

/// Handle an ApplyPatchApprovalRequest by consulting the parent session and replying.
async fn handle_patch_approval(
    chaos: &Chaos,
    _id: String,
    parent_session: &Arc<Session>,
    parent_ctx: &Arc<TurnContext>,
    event: ApplyPatchApprovalRequestEvent,
    cancel_token: &CancellationToken,
) {
    let ApplyPatchApprovalRequestEvent {
        call_id,
        changes,
        reason,
        grant_root,
        ..
    } = event;
    let approval_id = call_id.clone();
    let decision_rx = parent_session
        .request_patch_approval(parent_ctx, call_id, changes, reason, grant_root)
        .await;
    let decision = await_approval_with_cancel(
        async move { decision_rx.await.unwrap_or_default() },
        parent_session,
        &approval_id,
        cancel_token,
        /*review_cancel_token*/ None,
    )
    .await;
    let _ = chaos
        .submit(Op::PatchApproval {
            id: approval_id,
            decision,
        })
        .await;
}

async fn handle_request_user_input(
    chaos: &Chaos,
    id: String,
    parent_session: &Arc<Session>,
    parent_ctx: &Arc<TurnContext>,
    _pending_mcp_invocations: &Arc<Mutex<HashMap<String, McpInvocation>>>,
    event: RequestUserInputEvent,
    cancel_token: &CancellationToken,
) {
    let args = RequestUserInputArgs {
        questions: event.questions,
    };
    let response_fut =
        parent_session.request_user_input(parent_ctx, parent_ctx.sub_id.clone(), args);
    let response = await_user_input_with_cancel(
        response_fut,
        parent_session,
        &parent_ctx.sub_id,
        cancel_token,
    )
    .await;
    let _ = chaos.submit(Op::UserInputAnswer { id, response }).await;
}

async fn handle_request_permissions(
    chaos: &Chaos,
    parent_session: &Arc<Session>,
    parent_ctx: &Arc<TurnContext>,
    event: RequestPermissionsEvent,
    cancel_token: &CancellationToken,
) {
    let call_id = event.call_id;
    let args = RequestPermissionsArgs {
        reason: event.reason,
        permissions: event.permissions,
    };
    let response_fut = parent_session.request_permissions(parent_ctx, call_id.clone(), args);
    let response =
        await_request_permissions_with_cancel(response_fut, parent_session, &call_id, cancel_token)
            .await;
    let _ = chaos
        .submit(Op::RequestPermissionsResponse {
            id: call_id,
            response,
        })
        .await;
}

async fn await_user_input_with_cancel<F>(
    fut: F,
    parent_session: &Session,
    sub_id: &str,
    cancel_token: &CancellationToken,
) -> RequestUserInputResponse
where
    F: core::future::Future<Output = Option<RequestUserInputResponse>>,
{
    tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            let empty = RequestUserInputResponse {
                answers: HashMap::new(),
            };
            parent_session
                .notify_user_input_response(sub_id, empty.clone())
                .await;
            empty
        }
        response = fut => response.unwrap_or_else(|| RequestUserInputResponse {
            answers: HashMap::new(),
        }),
    }
}

async fn await_request_permissions_with_cancel<F>(
    fut: F,
    parent_session: &Session,
    call_id: &str,
    cancel_token: &CancellationToken,
) -> RequestPermissionsResponse
where
    F: core::future::Future<Output = Option<RequestPermissionsResponse>>,
{
    tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            let empty = RequestPermissionsResponse {
                permissions: Default::default(),
                scope: PermissionGrantScope::Turn,
            };
            parent_session
                .notify_request_permissions_response(call_id, empty.clone())
                .await;
            empty
        }
        response = fut => response.unwrap_or_else(|| RequestPermissionsResponse {
            permissions: Default::default(),
            scope: PermissionGrantScope::Turn,
        }),
    }
}

/// Await an approval decision, aborting on cancellation.
async fn await_approval_with_cancel<F>(
    fut: F,
    parent_session: &Session,
    approval_id: &str,
    cancel_token: &CancellationToken,
    review_cancel_token: Option<&CancellationToken>,
) -> chaos_ipc::protocol::ReviewDecision
where
    F: core::future::Future<Output = chaos_ipc::protocol::ReviewDecision>,
{
    tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            if let Some(review_cancel_token) = review_cancel_token {
                review_cancel_token.cancel();
            }
            parent_session
                .notify_approval(approval_id, chaos_ipc::protocol::ReviewDecision::Abort)
                .await;
            chaos_ipc::protocol::ReviewDecision::Abort
        }
        decision = fut => {
            decision
        }
    }
}

#[cfg(test)]
#[path = "chaos_delegate_tests.rs"]
mod tests;
