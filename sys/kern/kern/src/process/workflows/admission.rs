//! Non-wire host control. Caller JSON/_meta can never enqueue an admission.

use super::{WorkflowOwner, WorkflowScope, attach_with_operation};
use crate::chaos::{Session, SessionSettingsUpdate, TurnContext};
use crate::error::ChaosErr;
use crate::state::TaskKind;
use crate::tasks::{SessionTask, SessionTaskContext};
use chaos_ipc::background_tasks::WakePolicy;
use chaos_ipc::user_input::UserInput;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

const QUEUE_CAPACITY: usize = 8;

pub(crate) struct WorkflowControl {
    tx: async_channel::Sender<Request>,
    rx: async_channel::Receiver<Request>,
}

impl Default for WorkflowControl {
    fn default() -> Self {
        let (tx, rx) = async_channel::bounded(QUEUE_CAPACITY);
        Self { tx, rx }
    }
}

pub(crate) struct Request {
    session_id: String,
    owner: Option<WorkflowOwner>,
    closed: CancellationToken,
    reply: oneshot::Sender<Result<WorkflowScope, ChaosErr>>,
    span: tracing::Span,
}

impl WorkflowControl {
    pub(super) async fn open(&self, session_id: &str) -> Result<WorkflowScope, ChaosErr> {
        self.enqueue(session_id, None).await
    }

    pub(super) async fn open_owned(&self, owner: WorkflowOwner) -> Result<WorkflowScope, ChaosErr> {
        self.enqueue(&owner.0.session_id.clone(), Some(owner)).await
    }

    async fn enqueue(
        &self,
        session_id: &str,
        owner: Option<WorkflowOwner>,
    ) -> Result<WorkflowScope, ChaosErr> {
        let closed = CancellationToken::new();
        // Abandoning admission must not strand a native operation task.
        let guard = closed.clone().drop_guard();
        let (reply, result) = oneshot::channel();
        self.tx
            .try_send(Request {
                session_id: session_id.into(),
                owner,
                closed,
                reply,
                span: tracing::Span::current(),
            })
            .map_err(|_| invalid("workflow admission queue is full or closed"))?;
        let scope = tokio::time::timeout(std::time::Duration::from_secs(30), result)
            .await
            .map_err(|_| invalid("workflow admission timed out"))?
            .map_err(|_| invalid("workflow admission is unavailable"))??;
        guard.disarm();
        Ok(scope)
    }

    pub(crate) async fn recv(&self) -> Result<Request, async_channel::RecvError> {
        self.rx.recv().await
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.rx.is_closed()
    }

    pub(crate) fn shutdown(&self) {
        self.tx.close();
        while self.rx.try_recv().is_ok() {}
    }
}

impl Request {
    /// Called only by the process submission loop, never by a tool handler.
    pub(crate) async fn admit(self, session: &Arc<Session>, journal_paused: bool) {
        let result = self
            .start(session, journal_paused)
            .instrument(self.span.clone())
            .await;
        // A failed send drops the scope, ending an otherwise orphaned turn.
        let _ = self.reply.send(result);
    }

    async fn start(
        &self,
        session: &Arc<Session>,
        journal_paused: bool,
    ) -> Result<WorkflowScope, ChaosErr> {
        if self.closed.is_cancelled() || self.reply.is_closed() {
            return Err(invalid("workflow admission was abandoned"));
        }
        if journal_paused {
            return Err(invalid("journal recovery prevents workflow admission"));
        }
        {
            let state = session.state.lock().await;
            if state.machine_recovery.parked() || state.machine_recovery.requested_turn().is_some()
            {
                return Err(invalid("machine recovery prevents workflow admission"));
            }
        }
        if session.active_turn.lock().await.is_some() {
            return Err(invalid("workflow admission requires an idle process"));
        }
        if let Some(owner) = &self.owner {
            if owner.0.closed.is_cancelled() {
                return Err(invalid("workflow owner has closed"));
            }
            let bound = session
                .services
                .external_workflow_owner
                .get_or_init(|| owner.clone());
            if !bound.same_owner(owner) {
                return Err(invalid("workflow process belongs to another owner"));
            }
        } else if session.services.external_workflow_owner.get().is_some() {
            return Err(invalid("workflow process requires its bound owner"));
        }
        // Real turn registration inherits current constrained session policy.
        // No prompt, mode update, provider call or execution restore is involved.
        let turn = session
            .new_turn_with_sub_id(
                uuid::Uuid::new_v4().to_string(),
                SessionSettingsUpdate::default(),
            )
            .await
            .map_err(|error| invalid(error.to_string()))?;
        if self.closed.is_cancelled() || self.reply.is_closed() {
            let _ = session
                .permission_actor
                .remove_turn(turn.sub_id.clone())
                .await;
            turn.turn_metadata_state.cancel_git_enrichment_task();
            return Err(invalid("workflow admission was abandoned"));
        }
        // Background tool completion is data for the external host, never
        // permission to invoke the native model automatically after this task.
        session
            .services
            .internal_task_store
            .set_policy(WakePolicy::Interrupted)
            .await;
        if let Err(error) = session.checkpoint_background_tasks().await {
            let _ = session
                .permission_actor
                .remove_turn(turn.sub_id.clone())
                .await;
            turn.turn_metadata_state.cancel_git_enrichment_task();
            return Err(invalid(format!(
                "workflow journal admission failed: {error}"
            )));
        }
        if self.closed.is_cancelled() || self.reply.is_closed() {
            let _ = session
                .permission_actor
                .remove_turn(turn.sub_id.clone())
                .await;
            turn.turn_metadata_state.cancel_git_enrichment_task();
            return Err(invalid("workflow admission was abandoned"));
        }
        session
            .spawn_task(
                turn,
                Vec::new(),
                Operation {
                    closed: self.closed.clone(),
                },
            )
            .await;
        attach_with_operation(
            session,
            &self.session_id,
            Some(self.closed.clone()),
            self.owner.clone(),
        )
        .await
    }
}

/// A normal kernel lifecycle/approval context without the model/tool loop.
struct Operation {
    closed: CancellationToken,
}

impl SessionTask for Operation {
    fn kind(&self) -> TaskKind {
        TaskKind::Workflow
    }
    fn span_name(&self) -> &'static str {
        "external_workflow"
    }

    fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        turn: Arc<TurnContext>,
        _: Vec<UserInput>,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
        Box::pin(async move {
            session
                .clone_session()
                .send_event(
                    &turn,
                    chaos_ipc::protocol::EventMsg::TurnStarted(
                        chaos_ipc::protocol::TurnStartedEvent {
                            turn_id: turn.sub_id.clone(),
                            model_context_window: None,
                            collaboration_mode_kind: turn.collaboration_mode.mode,
                        },
                    ),
                )
                .await;
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => self.closed.cancel(),
                _ = self.closed.cancelled() => {},
            }
            None
        })
    }

    fn abort(
        &self,
        _: Arc<SessionTaskContext>,
        _: Arc<TurnContext>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.closed.cancel();
        Box::pin(async {})
    }
}

fn invalid(message: impl Into<String>) -> ChaosErr {
    ChaosErr::InvalidRequest(message.into())
}
