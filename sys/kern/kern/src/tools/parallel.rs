use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use chaos_traits::router::Adapter;
use chaos_traits::router::DEFAULT_ADAPTER_CAPACITY;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::instrument;
use tracing::trace_span;

use crate::chaos::Session;
use crate::chaos::TurnContext;
use crate::error::ChaosErr;
use crate::function_tool::FunctionCallError;
use crate::tools::context::AbortedToolOutput;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::registry::AnyToolResult;
use crate::tools::router::ToolCall;
use crate::tools::router::ToolCallSource;
use crate::tools::router::ToolRouter;
use chaos_ipc::models::ResponseInputItem;

#[derive(Clone)]
pub(crate) struct ToolCallRuntime {
    call_count: Arc<std::sync::atomic::AtomicUsize>,
    router: Arc<ToolRouter>,
    session: Arc<Session>,
    turn_context: Arc<TurnContext>,
    tracker: SharedTurnDiffTracker,
    scheduler: ToolSchedulerActor,
}

impl ToolCallRuntime {
    pub(crate) fn new(
        router: Arc<ToolRouter>,
        session: Arc<Session>,
        turn_context: Arc<TurnContext>,
        tracker: SharedTurnDiffTracker,
    ) -> Self {
        let scheduler = session.tool_scheduler().clone();
        Self {
            call_count: Arc::default(),
            router,
            session,
            turn_context,
            tracker,
            scheduler,
        }
    }

    pub(crate) fn call_count(&self) -> usize {
        self.call_count.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) fn handle_tool_call(
        self,
        call: ToolCall,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = Result<ResponseInputItem, ChaosErr>> {
        let future =
            self.handle_tool_call_with_source(call, ToolCallSource::Direct, cancellation_token);
        async move { future.await.map(AnyToolResult::into_response) }.in_current_span()
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) fn handle_tool_call_with_source(
        self,
        call: ToolCall,
        source: ToolCallSource,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = Result<AnyToolResult, ChaosErr>> {
        self.call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let supports_parallel = self.router.tool_supports_parallel(&call.tool_name);
        let workflow_leaf = matches!(source, ToolCallSource::Workflow { .. });
        let is_container = !workflow_leaf && self.router.is_execution_container(&call);
        let tasks = match &source {
            ToolCallSource::Workflow { tasks, .. } => Some(tasks.clone()),
            ToolCallSource::Direct => None,
        };
        let router = Arc::clone(&self.router);
        let session = Arc::clone(&self.session);
        let turn = Arc::clone(&self.turn_context);
        let tracker = Arc::clone(&self.tracker);
        let scheduler = self.scheduler;
        let started = Instant::now();

        let dispatch_span = trace_span!(
            "dispatch_tool_call",
            otel.name = call.tool_name.as_str(),
            tool_name = call.tool_name.as_str(),
            call_id = call.call_id.as_str(),
            aborted = false,
        );

        let dispatch = async move {
            let acquire = async {
                if is_container {
                    Ok(None)
                } else {
                    scheduler.acquire(supports_parallel).await.map(Some)
                }
            };

            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    let secs = started.elapsed().as_secs_f32().max(0.1);
                    dispatch_span.record("aborted", true);
                    Ok(Self::aborted_response(&call, secs))
                }
                lease = acquire => {
                    let mut lease = lease?;
                    let result = tokio::select! {
                        biased;
                        _ = cancellation_token.cancelled() => {
                            let secs = started.elapsed().as_secs_f32().max(0.1);
                            dispatch_span.record("aborted", true);
                            Ok(Self::aborted_response(&call, secs))
                        }
                        result = async {
                            let (mut router, mut turn) = if workflow_leaf {
                                Self::refresh_workflow_dispatch(
                                    &session, &turn, &cancellation_token,
                                ).await?
                            } else {
                                (router, turn)
                            };
                            // Visibility, mode, permissions and replay metadata may
                            // change while a leaf waits for its execution permit.
                            // Downgrade a stale parallel permit conservatively.
                            if workflow_leaf && supports_parallel
                                && !router.tool_supports_parallel(&call.tool_name)
                            {
                                drop(lease.take());
                                lease = Some(scheduler.acquire(false).await?);
                                (router, turn) = Self::refresh_workflow_dispatch(
                                    &session, &turn, &cancellation_token,
                                ).await?;
                            }
                            router.dispatch_tool_call(
                                session, turn, tracker, call.clone(), source,
                            ).instrument(dispatch_span.clone()).await
                        } => result,
                    };
                    result
                }
            }
        };
        // Account for existing dispatch tasks, without a second tree executor.
        let handle: AbortOnDropHandle<Result<AnyToolResult, FunctionCallError>> =
            AbortOnDropHandle::new(match tasks {
                Some(tasks) => tasks.spawn(dispatch),
                None => tokio::spawn(dispatch),
            });

        async move {
            match handle.await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(FunctionCallError::Fatal(message))) => Err(ChaosErr::Fatal(message)),
                Ok(Err(other)) => Err(ChaosErr::Fatal(other.to_string())),
                Err(err) => Err(ChaosErr::Fatal(format!(
                    "tool task failed to receive: {err:?}"
                ))),
            }
        }
        .in_current_span()
    }

    async fn refresh_workflow_dispatch(
        session: &Session,
        original_turn: &Arc<TurnContext>,
        cancellation: &CancellationToken,
    ) -> Result<(Arc<ToolRouter>, Arc<TurnContext>), FunctionCallError> {
        // Preserve the originating turn/tracker rather than synthesizing a new
        // turn with broader defaults. Effective context re-reads mode and grants.
        let turn = session
            .try_effective_turn_context(original_turn)
            .await
            .map_err(|error| {
                FunctionCallError::RespondToModel(format!("workflow turn is unavailable: {error}"))
            })?;
        let router = crate::chaos::built_tools(session, &turn, &[], cancellation)
            .await
            .map_err(|error| {
                FunctionCallError::RespondToModel(format!(
                    "workflow dispatch is unavailable: {error}"
                ))
            })?;
        Ok((router, turn))
    }
}

enum ToolSchedulerCommand {
    Enqueue {
        call_id: String,
        supports_parallel: bool,
        permit: oneshot::Sender<()>,
    },
}

struct QueuedToolCall {
    call_id: String,
    supports_parallel: bool,
    permit: oneshot::Sender<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolExecutionMode {
    Parallel,
    Exclusive,
}

#[derive(Default)]
struct ToolSchedule {
    queue: VecDeque<QueuedToolCall>,
    active: HashMap<String, ToolExecutionMode>,
}

impl ToolSchedule {
    fn enqueue(&mut self, call: QueuedToolCall) {
        self.queue.push_back(call);
        self.dispatch();
    }

    fn release(&mut self, call_id: &str) {
        self.queue.retain(|call| call.call_id != call_id);
        self.active.remove(call_id);
        self.dispatch();
    }

    fn dispatch(&mut self) {
        if self
            .active
            .values()
            .any(|mode| *mode == ToolExecutionMode::Exclusive)
        {
            return;
        }

        loop {
            let Some(next) = self.queue.front() else {
                return;
            };
            if !self.active.is_empty() && !next.supports_parallel {
                return;
            }

            let Some(next) = self.queue.pop_front() else {
                return;
            };
            if next.permit.send(()).is_err() {
                continue;
            }

            let mode = if next.supports_parallel {
                ToolExecutionMode::Parallel
            } else {
                ToolExecutionMode::Exclusive
            };
            assert!(self.active.insert(next.call_id, mode).is_none());
            if mode == ToolExecutionMode::Exclusive {
                return;
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ToolSchedulerActor {
    mailbox: Adapter<ToolSchedulerCommand, ()>,
    releases: mpsc::UnboundedSender<String>,
}

impl ToolSchedulerActor {
    pub(crate) fn spawn() -> Self {
        let (mailbox, mut commands) = Adapter::bounded(DEFAULT_ADAPTER_CAPACITY);
        let (releases, mut released_calls) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut schedule = ToolSchedule::default();
            loop {
                tokio::select! {
                    Some(packet) = commands.recv() => {
                        let ToolSchedulerCommand::Enqueue {
                            call_id,
                            supports_parallel,
                            permit,
                        } = packet.op;
                        schedule.enqueue(QueuedToolCall {
                            call_id,
                            supports_parallel,
                            permit,
                        });
                        let Some(reply) = packet.reply else {
                            panic!("tool scheduler enqueue requires a reply");
                        };
                        let _ = reply.send(());
                    }
                    Some(call_id) = released_calls.recv() => schedule.release(&call_id),
                    else => break,
                }
            }
        });
        Self { mailbox, releases }
    }

    pub(crate) async fn acquire(
        &self,
        supports_parallel: bool,
    ) -> Result<ToolExecutionLease, FunctionCallError> {
        // Caller IDs may repeat across turns/runtimes. Only this internal
        // ticket keys scheduling; public call IDs retain their provenance.
        let ticket = uuid::Uuid::new_v4().to_string();
        // Arm cleanup before enqueueing: dropping while awaiting the mailbox
        // acknowledgement must release an already queued/granted permit too.
        let lease = self.lease(ticket.clone());
        let (permit_tx, permit_rx) = oneshot::channel();
        self.enqueue(ticket, supports_parallel, permit_tx)
            .await
            .map_err(|error| FunctionCallError::Fatal(error.to_string()))?;
        permit_rx.await.map_err(|_| {
            FunctionCallError::Fatal("tool execution actor dropped dispatch permit".to_string())
        })?;
        Ok(lease)
    }

    async fn enqueue(
        &self,
        call_id: String,
        supports_parallel: bool,
        permit: oneshot::Sender<()>,
    ) -> Result<(), chaos_traits::router::AdapterError> {
        self.mailbox
            .call(ToolSchedulerCommand::Enqueue {
                call_id,
                supports_parallel,
                permit,
            })
            .await
    }

    fn lease(&self, call_id: String) -> ToolExecutionLease {
        ToolExecutionLease {
            call_id: Some(call_id),
            releases: self.releases.clone(),
        }
    }
}

pub(crate) struct ToolExecutionLease {
    call_id: Option<String>,
    releases: mpsc::UnboundedSender<String>,
}

impl Drop for ToolExecutionLease {
    fn drop(&mut self) {
        if let Some(call_id) = self.call_id.take() {
            let _ = self.releases.send(call_id);
        }
    }
}

impl ToolCallRuntime {
    fn aborted_response(call: &ToolCall, secs: f32) -> AnyToolResult {
        AnyToolResult {
            call_id: call.call_id.clone(),
            tool_name: call.tool_name.clone(),
            payload: call.payload.clone(),
            result: Box::new(AbortedToolOutput {
                message: Self::abort_message(call, secs),
            }),
        }
    }

    fn abort_message(call: &ToolCall, secs: f32) -> String {
        match call.tool_name.as_str() {
            "shell" | "container.exec" | "local_shell" | "shell_command" | "exec_command" => {
                format!("Wall time: {secs:.1} seconds\naborted by user")
            }
            _ => format!("aborted by user after {secs:.1}s"),
        }
    }
}

#[cfg(test)]
mod tests;
