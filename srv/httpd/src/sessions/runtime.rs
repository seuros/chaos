use std::future::Future;
use std::sync::Arc;

use chaos_ipc::protocol::{Event, EventMsg, Op, Submission};
use chaos_ipc::user_input::UserInput;
use chaos_kern::{Process, ProcessTable};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::warn;

use super::{
    ApiError, CLEANUP_GRACE, Command, CreateRequest, Session, SessionStatus, TurnRequest,
    TurnStatus,
};
use crate::ServerState;

// A small typed backend keeps lifecycle tests deterministic, without booting a
// real process, model client, daemon or network service.
pub(super) trait Backend: Send + Sync + 'static {
    fn next_event(&self) -> impl Future<Output = Result<Event, ()>> + Send;
    fn execute(
        &self,
        id: String,
        request: TurnRequest,
    ) -> impl Future<Output = Result<(), ()>> + Send;
    fn steer(&self, id: String, text: String) -> impl Future<Output = Result<(), ApiError>> + Send;
    fn interrupt(&self) -> impl Future<Output = Result<(), ()>> + Send;
    /// Replace or withdraw the session's host notice. Never blocks on the model.
    fn notice(&self, notice: Option<chaos_kern::host_notice::HostNotice>);
    fn shutdown(&self) -> impl Future<Output = Result<(), ()>> + Send;
    fn is_quiescent(&self) -> bool {
        true
    }
    fn activity_changed(&self) -> impl Future<Output = Result<(), ()>> + Send {
        std::future::pending()
    }
}

struct KernelBackend {
    process: Arc<Process>,
    table: Arc<ProcessTable>,
    process_id: chaos_ipc::ProcessId,
    activity: tokio::sync::Mutex<
        tokio::sync::watch::Receiver<chaos_ipc::background_tasks::ProcessActivity>,
    >,
}

// A cancelled/panicking supervisor must not leave a falsely healthy record or
// declare external side effects safe to replay. Keep reservations in the record.
pub(super) struct SupervisorGuard(pub Arc<Session>);

impl Drop for SupervisorGuard {
    fn drop(&mut self) {
        let mut data = self.0.data.lock().unwrap();
        if matches!(data.status, SessionStatus::Closed | SessionStatus::Failed) {
            return;
        }
        data.mark_unknown();
        drop(data);
        self.0.notify();
    }
}

fn input(text: String) -> Vec<UserInput> {
    vec![UserInput::Text {
        text,
        text_elements: Vec::new(),
    }]
}

impl Backend for KernelBackend {
    async fn next_event(&self) -> Result<Event, ()> {
        self.process.next_event().await.map_err(|_| ())
    }

    async fn execute(&self, id: String, request: TurnRequest) -> Result<(), ()> {
        if !self.is_quiescent() {
            // Never merge a newly admitted HTTP turn into autonomous work
            // that raced admission. There has been no prompt submission yet.
            return Err(());
        }
        // A notice belongs to the turn it was posted for; never carry an
        // undelivered one into the next turn.
        self.process.set_host_notice(None);
        let mut op = self.process.user_turn(input(request.request), None).await;
        if let Op::UserTurn {
            model,
            effort,
            collaboration_mode,
            ..
        } = &mut op
        {
            if let Some(selected) = request.model {
                *model = selected.clone();
                if let Some(mode) = collaboration_mode {
                    mode.settings.model = selected;
                }
            }
            if let Some(selected) = request.reasoning_effort {
                *effort = Some(selected);
                if let Some(mode) = collaboration_mode {
                    mode.settings.reasoning_effort = Some(selected);
                }
            }
        }
        self.process
            .submit_with_id(Submission {
                id,
                op,
                trace: None,
            })
            .await
            .map_err(|_| ())
    }

    async fn steer(&self, id: String, text: String) -> Result<(), ApiError> {
        self.process
            .steer_input(input(text), Some(&id))
            .await
            .map(|_| ())
            .map_err(|_| ApiError::conflict("expected turn is no longer active"))
    }

    fn notice(&self, notice: Option<chaos_kern::host_notice::HostNotice>) {
        self.process.set_host_notice(notice);
    }

    async fn interrupt(&self) -> Result<(), ()> {
        self.process
            .submit(Op::Interrupt)
            .await
            .map(|_| ())
            .map_err(|_| ())
    }

    async fn shutdown(&self) -> Result<(), ()> {
        self.process.shutdown_and_wait().await.map_err(|_| ())?;
        self.table.remove_process(&self.process_id).await;
        Ok(())
    }

    fn is_quiescent(&self) -> bool {
        self.process.subscribe_activity().borrow().is_quiescent()
    }

    async fn activity_changed(&self) -> Result<(), ()> {
        self.activity.lock().await.changed().await.map_err(|_| ())
    }
}

pub(super) fn spawn(
    state: Arc<ServerState>,
    session: Arc<Session>,
    commands: mpsc::Receiver<Command>,
    request: CreateRequest,
) {
    let ttl = state.sessions.as_ref().unwrap().ttl;
    let expires = Instant::now() + ttl;
    // Spawn BEFORE returning acceptance. The task owns startup/execution even
    // if the HTTP caller drops its response or disconnects.
    tokio::spawn(async move {
        let _owner = SupervisorGuard(session.clone());
        let config = state.config.as_ref().clone();
        let started = match request.resume_process_id {
            Some(id) => {
                state
                    .process_table
                    .resume_process(config, id, state.auth_manager.clone(), None)
                    .await
            }
            None => state.process_table.start_process(config).await,
        };
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                warn!(session_id = %session.id, %error, "HTTP session startup failed");
                session.data.lock().unwrap().status = SessionStatus::Failed;
                session.notify();
                return;
            }
        };
        let (process_id, process, configured) = started.into_parts();
        {
            let mut data = session.data.lock().unwrap();
            data.process_id = Some(process_id.to_string());
            data.observe(
                &Event {
                    id: String::new(),
                    msg: EventMsg::SessionConfigured(configured),
                },
                false,
            );
            // A close request may already be queued during startup.
            if data.status == SessionStatus::Starting {
                data.status = if process.subscribe_activity().borrow().is_quiescent() {
                    SessionStatus::Idle
                } else {
                    SessionStatus::Running
                };
            }
        }
        session.notify();
        let backend = KernelBackend {
            activity: tokio::sync::Mutex::new(process.subscribe_activity()),
            process,
            table: state.process_table.clone(),
            process_id,
        };
        supervise(session, commands, backend, expires).await;
    });
}

pub(super) async fn supervise<B: Backend>(
    session: Arc<Session>,
    mut commands: mpsc::Receiver<Command>,
    backend: B,
    expires: Instant,
) {
    let _owner = SupervisorGuard(session.clone());
    loop {
        let deadline = {
            let data = session.data.lock().unwrap();
            data.active
                .as_ref()
                .map(|active| active.deadline.min(expires))
                .unwrap_or(expires)
        };
        tokio::select! {
            // Deadlines must not be starved by an always-ready event stream.
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                // Failed containment never releases execution capacity. Continue
                // accepting explicit close retries; don't replay the old turn.
                terminate(&session, &mut commands, &backend, TurnStatus::TimedOut).await;
                break;
            }
            command = commands.recv() => {
                match command {
                    Some(Command::Turn(id)) => {
                        let (request, turn_deadline, interrupted) = {
                            let mut data = session.data.lock().unwrap();
                            let turn_deadline = data.active.as_ref().unwrap().deadline.min(expires);
                            let turn = data.turns.get_mut(&id).unwrap();
                            let interrupted = turn.snapshot.status == TurnStatus::Interrupting;
                            (turn.request.take().unwrap(), turn_deadline, interrupted)
                        };
                        if session.data.lock().unwrap().status == SessionStatus::Closing {
                            terminate(&session, &mut commands, &backend, TurnStatus::Interrupted).await;
                            break;
                        }
                        if interrupted {
                            session.data.lock().unwrap().finish_turn(&id, TurnStatus::Interrupted);
                            session.notify();
                            continue;
                        }
                        let executed = tokio::time::timeout_at(turn_deadline, backend.execute(id, request)).await;
                        if !matches!(executed, Ok(Ok(()))) {
                            let outcome = if executed.is_err() { TurnStatus::TimedOut } else { TurnStatus::Unknown };
                            terminate(&session, &mut commands, &backend, outcome).await;
                            break;
                        }
                    }
                    Some(Command::Steer { turn_id, text, reply }) => {
                        let active = session.data.lock().unwrap().require_steerable(&turn_id);
                        let result = match active {
                            Ok(()) => tokio::time::timeout_at(deadline, backend.steer(turn_id, text))
                                .await
                                .unwrap_or(Err(ApiError::conflict("steering deadline exceeded; reconcile delivery"))),
                            Err(error) => Err(error),
                        };
                        let _ = reply.send(result);
                    }
                    Some(Command::Notice { turn_id, notice, reply }) => {
                        // Withdrawal is always honoured; setting needs the turn
                        // to still be the active foreground one.
                        let result = match &notice {
                            Some(_) => session.data.lock().unwrap().require_steerable(&turn_id),
                            None => Ok(()),
                        };
                        if result.is_ok() {
                            backend.notice(notice);
                        }
                        let _ = reply.send(result);
                    }
                    Some(Command::Interrupt(id)) => {
                        // A queued interrupt cannot hit a later HTTP turn: that
                        // turn cannot be admitted before we consume its predecessor's
                        // matching terminal event.
                        let active = session.data.lock().unwrap().require_active(&id).is_ok();
                        if active && !matches!(tokio::time::timeout_at(deadline, backend.interrupt()).await, Ok(Ok(()))) {
                            terminate(&session, &mut commands, &backend, TurnStatus::Unknown).await;
                            break;
                        }
                    }
                    Some(Command::Close) | None => {
                        terminate(&session, &mut commands, &backend, TurnStatus::Interrupted).await;
                        break;
                    }
                }
            }
            event = backend.next_event() => {
                match event {
                    Ok(event) => {
                        let shutdown = matches!(event.msg, EventMsg::ShutdownComplete);
                        session.observe_with_activity(&event, backend.is_quiescent());
                        if shutdown {
                            terminate(&session, &mut commands, &backend, TurnStatus::Unknown).await;
                            break;
                        }
                    }
                    Err(()) => {
                        terminate(&session, &mut commands, &backend, TurnStatus::Unknown).await;
                        break;
                    }
                }
            }
            activity = backend.activity_changed() => {
                if activity.is_err() {
                    terminate(&session, &mut commands, &backend, TurnStatus::Unknown).await;
                    break;
                }
                session.data.lock().unwrap().maybe_finish(backend.is_quiescent());
                session.notify();
            }
        }
    }
}

async fn terminate<B: Backend>(
    session: &Session,
    commands: &mut mpsc::Receiver<Command>,
    backend: &B,
    outcome: TurnStatus,
) {
    if !stop(session, backend, outcome).await {
        unknown_loop(session, commands, backend).await;
    }
}

async fn stop<B: Backend>(session: &Session, backend: &B, outcome: TurnStatus) -> bool {
    session.data.lock().unwrap().status = SessionStatus::Closing;
    let stopped = matches!(
        tokio::time::timeout(CLEANUP_GRACE, backend.shutdown()).await,
        Ok(Ok(()))
    );
    let mut data = session.data.lock().unwrap();
    if stopped {
        if let Some(id) = data.active.as_ref().map(|active| active.id.clone()) {
            data.finish_turn(&id, outcome);
        }
        data.status = SessionStatus::Closed;
    } else {
        data.mark_unknown();
    }
    drop(data);
    session.notify();
    stopped
}

async fn unknown_loop<B: Backend>(
    session: &Session,
    commands: &mut mpsc::Receiver<Command>,
    backend: &B,
) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::Close if stop(session, backend, TurnStatus::Unknown).await => break,
            Command::Steer { reply, .. } | Command::Notice { reply, .. } => {
                let _ = reply.send(Err(ApiError::conflict("session outcome is unknown")));
            }
            _ => {}
        }
    }
}
