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
        Self {
            router,
            session,
            turn_context,
            tracker,
            scheduler: ToolSchedulerActor::spawn(),
        }
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
        let supports_parallel = self.router.tool_supports_parallel(&call.tool_name);
        let router = Arc::clone(&self.router);
        let session = Arc::clone(&self.session);
        let turn = Arc::clone(&self.turn_context);
        let tracker = Arc::clone(&self.tracker);
        let scheduler = self.scheduler.clone();
        let started = Instant::now();

        let dispatch_span = trace_span!(
            "dispatch_tool_call",
            otel.name = call.tool_name.as_str(),
            tool_name = call.tool_name.as_str(),
            call_id = call.call_id.as_str(),
            aborted = false,
        );

        let handle: AbortOnDropHandle<Result<AnyToolResult, FunctionCallError>> =
            AbortOnDropHandle::new(tokio::spawn(async move {
                let (permit_tx, permit_rx) = oneshot::channel();
                scheduler
                    .enqueue(call.call_id.clone(), supports_parallel, permit_tx)
                    .await
                    .map_err(|err| FunctionCallError::Fatal(err.to_string()))?;
                let _lease = scheduler.lease(call.call_id.clone());

                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        let secs = started.elapsed().as_secs_f32().max(0.1);
                        dispatch_span.record("aborted", true);
                        Ok(Self::aborted_response(&call, secs))
                    }
                    permit = permit_rx => {
                        permit.map_err(|_| FunctionCallError::Fatal(
                            "tool execution actor dropped dispatch permit".to_string()
                        ))?;
                        let result = tokio::select! {
                            _ = cancellation_token.cancelled() => {
                                let secs = started.elapsed().as_secs_f32().max(0.1);
                                dispatch_span.record("aborted", true);
                                Ok(Self::aborted_response(&call, secs))
                            }
                            result = router
                                .dispatch_tool_call(
                                    session,
                                    turn,
                                    tracker,
                                    call.clone(),
                                    source,
                                )
                                .instrument(dispatch_span.clone()) => result,
                        };
                        result
                    }
                }
            }));

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

            let next = self.queue.pop_front().expect("queued tool call exists");
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
struct ToolSchedulerActor {
    mailbox: Adapter<ToolSchedulerCommand, ()>,
    releases: mpsc::UnboundedSender<String>,
}

impl ToolSchedulerActor {
    fn spawn() -> Self {
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
                        let _ = packet
                            .reply
                            .expect("tool scheduler enqueue requires a reply")
                            .send(());
                    }
                    Some(call_id) = released_calls.recv() => schedule.release(&call_id),
                    else => break,
                }
            }
        });
        Self { mailbox, releases }
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

struct ToolExecutionLease {
    call_id: Option<String>,
    releases: mpsc::UnboundedSender<String>,
}

impl Drop for ToolExecutionLease {
    fn drop(&mut self) {
        self.releases
            .send(self.call_id.take().expect("tool lease already released"))
            .expect("tool scheduler actor stopped while a call was active");
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;

    #[test]
    fn exclusive_waits_for_active_parallel_calls() {
        let mut schedule = ToolSchedule::default();
        schedule
            .active
            .insert("running".to_string(), ToolExecutionMode::Parallel);
        let (exclusive_tx, mut exclusive_rx) = oneshot::channel();
        schedule.queue.push_back(QueuedToolCall {
            call_id: "exclusive".to_string(),
            supports_parallel: false,
            permit: exclusive_tx,
        });

        schedule.dispatch();

        assert_eq!(schedule.queue.len(), 1);
        assert!(exclusive_rx.try_recv().is_err());
        assert_eq!(schedule.active.len(), 1);
    }

    #[test]
    fn leading_parallel_calls_are_released_together() {
        let mut schedule = ToolSchedule::default();
        let (first_tx, mut first_rx) = oneshot::channel();
        let (second_tx, mut second_rx) = oneshot::channel();
        let (exclusive_tx, mut exclusive_rx) = oneshot::channel();
        schedule.queue.push_back(QueuedToolCall {
            call_id: "first".to_string(),
            supports_parallel: true,
            permit: first_tx,
        });
        schedule.queue.push_back(QueuedToolCall {
            call_id: "second".to_string(),
            supports_parallel: true,
            permit: second_tx,
        });
        schedule.queue.push_back(QueuedToolCall {
            call_id: "exclusive".to_string(),
            supports_parallel: false,
            permit: exclusive_tx,
        });

        schedule.dispatch();

        assert_eq!(first_rx.try_recv(), Ok(()));
        assert_eq!(second_rx.try_recv(), Ok(()));
        assert!(exclusive_rx.try_recv().is_err());
        assert_eq!(schedule.active.len(), 2);
        assert_eq!(schedule.queue.len(), 1);
    }

    #[test]
    fn releasing_a_call_dispatches_the_next_call() {
        let mut schedule = ToolSchedule::default();
        let (first_tx, mut first_rx) = oneshot::channel();
        let (second_tx, mut second_rx) = oneshot::channel();
        schedule.enqueue(QueuedToolCall {
            call_id: "first".to_string(),
            supports_parallel: false,
            permit: first_tx,
        });
        schedule.enqueue(QueuedToolCall {
            call_id: "second".to_string(),
            supports_parallel: false,
            permit: second_tx,
        });

        assert_eq!(first_rx.try_recv(), Ok(()));
        assert!(second_rx.try_recv().is_err());

        schedule.release("first");
        assert_eq!(second_rx.try_recv(), Ok(()));
        assert_eq!(
            schedule.active.get("second"),
            Some(&ToolExecutionMode::Exclusive)
        );
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
            "shell" | "container.exec" | "local_shell" | "shell_command" | "unified_exec" => {
                format!("Wall time: {secs:.1} seconds\naborted by user")
            }
            _ => format!("aborted by user after {secs:.1}s"),
        }
    }
}
