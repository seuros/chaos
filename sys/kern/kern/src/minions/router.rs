//! Process-table router — the first satellite in the chaos domain to
//! adopt the typed mailbox pattern from `chaos-traits::router`.
//!
//! The router holds a `Weak<ProcessTableState>` and upgrades on each
//! packet. A strong pointer would form a cycle back to the adapter
//! (state → processes → Process → Session → AgentControl → Adapter
//! sender) and block drop-driven cleanup at shutdown.
//!
//! Mutation handlers dispatch to background subtasks in a `JoinSet`,
//! so the router loop keeps pumping under concurrent spawns — the
//! mailbox serializes ingress, not body execution. Each subtask is
//! instrumented with the packet's W3C trace path so OTel spans
//! produced inside the spawn re-parent to the caller across the hop.
//!
//! `Drain` joins every currently-dispatched subtask before acking.
//! This only covers the routed body (the state mutation itself) —
//! post-reply work in the caller (slot commit, completion-watcher
//! spawn, initial `Op::UserInput` submission) is not covered and
//! must be waited on separately by the turn-boundary handler.
//!
//! Read paths (`get_process`, `list_process_ids`, `send_op`, etc.)
//! stay direct against `ProcessTableState` — routing reads through a
//! mailbox adds latency for no correctness gain.

use std::sync::Arc;
use std::sync::Weak;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::W3cTraceContext;
use chaos_traits::Adapter;
use chaos_traits::DEFAULT_ADAPTER_CAPACITY;
use chaos_traits::Packet;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tracing::Instrument;
use tracing::warn;

use crate::AuthManager;
use crate::config::Config;
use crate::error::Result as ChaosResult;
use crate::minions::control::AgentControl;
use crate::process_table::NewProcess;
use crate::process_table::ProcessTableState;
use crate::shell_snapshot::ShellSnapshot;

/// Typed args for spawning a fresh process.
pub(crate) struct SpawnArgs {
    pub(crate) config: Config,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) agent_control: AgentControl,
    pub(crate) session_source: SessionSource,
    pub(crate) persist_extended_history: bool,
    pub(crate) metrics_service_name: Option<String>,
    pub(crate) inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
    pub(crate) parent_trace: Option<W3cTraceContext>,
}

/// Typed args for resuming an existing process from journal history.
pub(crate) struct ResumeArgs {
    pub(crate) config: Config,
    pub(crate) process_id: ProcessId,
    pub(crate) agent_control: AgentControl,
    pub(crate) session_source: SessionSource,
    pub(crate) inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
}

/// Typed args for forking a process from captured history.
pub(crate) struct ForkArgs {
    pub(crate) config: Config,
    pub(crate) initial_history: InitialHistory,
    pub(crate) agent_control: AgentControl,
    pub(crate) session_source: SessionSource,
    pub(crate) persist_extended_history: bool,
    pub(crate) inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
}

/// Packets accepted by the process-table router. The reply sender
/// rides inside each variant rather than on the enclosing `Packet`
/// because different ops return different reply payloads.
pub(crate) enum ProcessTableOp {
    Spawn {
        args: Box<SpawnArgs>,
        reply: oneshot::Sender<ChaosResult<NewProcess>>,
    },
    Resume {
        args: Box<ResumeArgs>,
        reply: oneshot::Sender<ChaosResult<NewProcess>>,
    },
    Fork {
        args: Box<ForkArgs>,
        reply: oneshot::Sender<ChaosResult<NewProcess>>,
    },
    /// Wait until every packet already observed by the router has
    /// fully completed, then ack. Consumed at turn boundaries before
    /// `TurnAborted` is emitted; the wiring lands alongside that
    /// handler in a follow-up.
    #[allow(dead_code, reason = "wired by the turn-boundary drain follow-up")]
    Drain { reply: oneshot::Sender<()> },
}

/// Handle to a spawned process-table router. Owns the adapter that
/// producers clone.
pub(crate) struct ProcessTableRouter {
    adapter: Adapter<ProcessTableOp>,
}

impl ProcessTableRouter {
    /// Spawn a router task that serves the provided state and return
    /// the paired handle.
    ///
    /// The router only holds a `Weak<ProcessTableState>`. The process
    /// table's registry transitively owns every `AgentControl`, which in
    /// turn owns a clone of this adapter's sender — holding a strong
    /// state pointer here would form a reference cycle that blocks
    /// shell-snapshot cleanup (and anything else driven by state drop)
    /// at shutdown.
    pub(crate) fn enumerate(state: Arc<ProcessTableState>) -> Self {
        let (adapter, rx) = Adapter::<ProcessTableOp>::bounded(DEFAULT_ADAPTER_CAPACITY);
        let weak_state = Arc::downgrade(&state);
        drop(state);
        let span = tracing::info_span!("router.process_table");
        tokio::spawn(router_loop(weak_state, rx).instrument(span));
        Self { adapter }
    }

    pub(crate) fn adapter(&self) -> Adapter<ProcessTableOp> {
        self.adapter.clone()
    }
}

async fn router_loop(
    state: Weak<ProcessTableState>,
    mut rx: mpsc::Receiver<Packet<ProcessTableOp>>,
) {
    let mut in_flight: JoinSet<()> = JoinSet::new();

    while let Some(packet) = rx.recv().await {
        // Passively reap any subtasks that finished since the last packet.
        while in_flight.try_join_next().is_some() {}

        // Upgrade once per packet. If the state is gone, close the
        // mailbox so queued and pending producers fail fast — then
        // drop this packet's reply (returning `ReplyDropped` to its
        // caller) and exit the loop into the final join drain.
        let Some(strong_state) = state.upgrade() else {
            rx.close();
            drop(packet);
            break;
        };

        let span = span_from_packet_path(packet.path.as_ref());
        match packet.op {
            ProcessTableOp::Spawn { args, reply } => {
                let state = strong_state;
                in_flight.spawn(
                    async move {
                        let SpawnArgs {
                            config,
                            auth_manager,
                            agent_control,
                            session_source,
                            persist_extended_history,
                            metrics_service_name,
                            inherited_shell_snapshot,
                            parent_trace,
                        } = *args;
                        let result = Box::pin(state.spawn_process_with_source(
                            config,
                            InitialHistory::New,
                            auth_manager,
                            agent_control,
                            session_source,
                            Vec::new(),
                            persist_extended_history,
                            metrics_service_name,
                            inherited_shell_snapshot,
                            parent_trace,
                        ))
                        .await;
                        if reply.send(result).is_err() {
                            warn!("ProcessTableOp::Spawn caller dropped reply");
                        }
                    }
                    .instrument(span),
                );
            }
            ProcessTableOp::Resume { args, reply } => {
                let state = strong_state;
                in_flight.spawn(
                    async move {
                        let ResumeArgs {
                            config,
                            process_id,
                            agent_control,
                            session_source,
                            inherited_shell_snapshot,
                        } = *args;
                        let result = Box::pin(state.resume_process_with_source(
                            config,
                            process_id,
                            agent_control,
                            session_source,
                            inherited_shell_snapshot,
                        ))
                        .await;
                        if reply.send(result).is_err() {
                            warn!("ProcessTableOp::Resume caller dropped reply");
                        }
                    }
                    .instrument(span),
                );
            }
            ProcessTableOp::Fork { args, reply } => {
                let state = strong_state;
                in_flight.spawn(
                    async move {
                        let ForkArgs {
                            config,
                            initial_history,
                            agent_control,
                            session_source,
                            persist_extended_history,
                            inherited_shell_snapshot,
                        } = *args;
                        let result = Box::pin(state.fork_process_with_source(
                            config,
                            initial_history,
                            agent_control,
                            session_source,
                            persist_extended_history,
                            inherited_shell_snapshot,
                        ))
                        .await;
                        if reply.send(result).is_err() {
                            warn!("ProcessTableOp::Fork caller dropped reply");
                        }
                    }
                    .instrument(span),
                );
            }
            ProcessTableOp::Drain { reply } => {
                drop(strong_state);
                while in_flight.join_next().await.is_some() {}
                if reply.send(()).is_err() {
                    warn!("ProcessTableOp::Drain caller dropped reply");
                }
            }
        }
    }

    // Channel closed or state gone — drain remaining in-flight work
    // so callers awaiting a reply are not left hanging.
    while in_flight.join_next().await.is_some() {}
}

/// Build an instrumentation span that re-parents to the caller's W3C
/// trace carrier (if any), so OTel exports see one logical trace
/// spanning the mailbox hop.
fn span_from_packet_path(path: Option<&chaos_ipc::protocol::W3cTraceContext>) -> tracing::Span {
    let span = tracing::info_span!("router.process_table.handle");
    if let Some(path) = path {
        chaos_syslog::set_parent_from_w3c_trace_context(&span, path);
    }
    span
}
