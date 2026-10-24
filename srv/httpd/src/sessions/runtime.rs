use std::future::Future;
use std::sync::Arc;

use chaos_ipc::protocol::{Event, EventMsg, Op, Submission};
use chaos_ipc::user_input::UserInput;
use chaos_kern::{Process, ProcessTable};
use chaos_snitch::tracing::warn;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{
    ApiError, CLEANUP_GRACE, Command, CreateRequest, NoticeChange, Session, SessionStatus,
    TurnRequest, TurnStatus,
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
    /// Offer a host notice; an id already seen is ignored. Never blocks on the model.
    fn post_notice(&self, notice: chaos_kern::host_notice::HostNotice) -> bool;
    /// Withdraw the pending host notice, optionally only if it has this id.
    fn withdraw_notice(&self, id: Option<String>) -> bool;
    fn shutdown(&self) -> impl Future<Output = Result<(), ()>> + Send;
    fn is_quiescent(&self) -> bool {
        true
    }
    fn confirm_quiescent(&self) -> impl Future<Output = bool> + Send {
        async { self.is_quiescent() }
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
        self.0.done.send_replace(true);
        let Ok(mut data) = self.0.lock_data() else {
            // lock_data abandons ownership; retain capacity and do not panic
            // again if the supervisor is already unwinding.
            self.0.notify();
            return;
        };
        if matches!(data.status, SessionStatus::Closed | SessionStatus::Failed) {
            return;
        }
        data.mark_unknown();
        drop(data);
        if let Some(owner) = self.0.ledger.get() {
            owner.abandon();
        }
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
        if !self.confirm_quiescent().await {
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

    fn post_notice(&self, notice: chaos_kern::host_notice::HostNotice) -> bool {
        self.process.post_host_notice(notice)
    }

    fn withdraw_notice(&self, id: Option<String>) -> bool {
        self.process.withdraw_host_notice(id.as_deref())
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
        self.table
            .shutdown_descendants_and_wait(self.process_id)
            .await
            .map_err(|_| ())?;
        self.table.remove_process(&self.process_id).await;
        Ok(())
    }

    fn is_quiescent(&self) -> bool {
        self.process.subscribe_activity().borrow().is_quiescent()
    }

    async fn confirm_quiescent(&self) -> bool {
        self.is_quiescent()
            && matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    self.table.process_tree_is_quiescent(self.process_id),
                )
                .await,
                Ok(Ok(true))
            )
    }

    async fn activity_changed(&self) -> Result<(), ()> {
        self.activity.lock().await.changed().await.map_err(|_| ())
    }
}

pub(crate) fn spawn(
    state: Arc<ServerState>,
    session: Arc<Session>,
    mut commands: mpsc::Receiver<Command>,
    request: CreateRequest,
    lazy: bool,
) {
    let ttl = state.sessions.as_ref().unwrap().ttl;
    let expires = Instant::now() + ttl;
    // Spawn BEFORE returning acceptance. The task owns startup/execution even
    // if the HTTP caller drops its response or disconnects.
    tokio::spawn(async move {
        let _owner = SupervisorGuard(session.clone());
        let first = if lazy {
            let command = tokio::select! {
                _ = session.ownership_lost() => { return; }
                _ = session.daemon_stopping() => Some(Command::Suspend),
                _ = tokio::time::sleep_until(expires) => Some(Command::Close),
                command = commands.recv() => command,
            };
            match command {
                Some(command @ Command::Turn(_)) => Some(command),
                command => {
                    let suspend = matches!(command, Some(Command::Suspend));
                    let result = if let Some(owner) = session.ledger.get() {
                        owner.retire(suspend).await
                    } else {
                        Ok(())
                    };
                    if result.is_ok() {
                        session.data.lock().unwrap().status = SessionStatus::Closed;
                    }
                    session.notify();
                    return;
                }
            }
        } else {
            if let Some(owner) = session.ledger.get() {
                // Record uncertainty BEFORE starting a kernel, even when its
                // newly generated process ID is not yet known.
                if let Some(id) = request.resume_process_id
                    && owner.bind(id).await.is_err()
                {
                    return;
                }
                if owner.quarantine().await.is_err() {
                    return;
                }
            }
            None
        };
        if let Some(owner) = session.ledger.get()
            && owner.check_execution().is_err()
        {
            return;
        }
        let config = state
            .sessions
            .as_ref()
            .and_then(|manager| manager.pinned_config())
            .unwrap_or_else(|| state.config.clone())
            .as_ref()
            .clone();
        let auth = state
            .sessions
            .as_ref()
            .and_then(|manager| manager.pinned_auth())
            .unwrap_or_else(|| state.auth_manager.clone());
        let started = match request.resume_process_id {
            Some(id) => {
                state
                    .process_table
                    .resume_process(config, id, auth, None)
                    .await
            }
            None => {
                state
                    .process_table
                    .start_process_with_auth(config, auth)
                    .await
            }
        };
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                warn!(session_id = %session.id, %error, "HTTP session startup failed");
                session.data.lock().unwrap().status = if session.ledger.get().is_some() {
                    SessionStatus::Unknown
                } else {
                    SessionStatus::Failed
                };
                session.notify();
                return;
            }
        };
        let (process_id, process, configured) = started.into_parts();
        let backend = KernelBackend {
            activity: tokio::sync::Mutex::new(process.subscribe_activity()),
            process: process.clone(),
            table: state.process_table.clone(),
            process_id,
        };
        if let Some(owner) = session.ledger.get() {
            let ready = async {
                if !lazy {
                    if request.resume_process_id.is_some() {
                        owner.bind(process_id).await?;
                    } else {
                        owner.bind_new(process_id).await?;
                    }
                    if !backend.confirm_quiescent().await {
                        return Err(ApiError::conflict(
                            "autonomous startup requires containment",
                        ));
                    }
                    owner.reconcile_idle().await?;
                }
                process
                    .set_provider_invocation_sink(owner.invocation_sink())
                    .map_err(|_| {
                        ApiError::conflict("provider accounting could not be installed")
                    })?;
                Ok::<_, ApiError>(())
            }
            .await;
            if ready.is_err() {
                terminate(
                    &session,
                    &mut commands,
                    &backend,
                    TurnStatus::Unknown,
                    false,
                )
                .await;
                return;
            }
        }
        session.data.lock().unwrap().process_id = Some(process_id.to_string());
        if session
            .observe_committed(
                &Event {
                    id: String::new(),
                    msg: EventMsg::SessionConfigured(configured),
                },
                false,
            )
            .await
            .is_err()
        {
            terminate(
                &session,
                &mut commands,
                &backend,
                TurnStatus::Unknown,
                false,
            )
            .await;
            return;
        }
        {
            let mut data = session.data.lock().unwrap();
            data.orphan_permit = None;
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
        supervise_inner(session, commands, backend, expires, first).await;
    });
}

#[cfg(test)]
pub(super) async fn supervise<B: Backend>(
    session: Arc<Session>,
    commands: mpsc::Receiver<Command>,
    backend: B,
    expires: Instant,
) {
    supervise_inner(session, commands, backend, expires, None).await;
}

async fn supervise_inner<B: Backend>(
    session: Arc<Session>,
    mut commands: mpsc::Receiver<Command>,
    backend: B,
    expires: Instant,
    mut first: Option<Command>,
) {
    let _owner = SupervisorGuard(session.clone());
    // Which HTTP turn posted the pending host notice, if any.
    let mut notice_owner: Option<String> = None;
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
            _ = session.ownership_lost() => {
                terminate(&session, &mut commands, &backend, TurnStatus::Unknown, false).await;
                break;
            }
            _ = session.daemon_stopping() => {
                terminate(
                    &session, &mut commands, &backend,
                    TurnStatus::Interrupted, session.ledger.get().is_some(),
                ).await;
                break;
            }
            _ = tokio::time::sleep_until(deadline) => {
                // Failed containment never releases execution capacity. Continue
                // accepting explicit close retries; don't replay the old turn.
                terminate(&session, &mut commands, &backend, TurnStatus::TimedOut, false).await;
                break;
            }
            command = async {
                if first.is_some() { first.take() } else { commands.recv().await }
            } => {
                match command {
                    Some(Command::Turn(id)) => {
                        // The backend clears any undelivered notice when it
                        // admits the turn; forget the previous owner too.
                        notice_owner = None;
                        let (request, turn_deadline, interrupted) = {
                            let mut data = session.data.lock().unwrap();
                            let turn_deadline = data.active.as_ref().unwrap().deadline.min(expires);
                            let turn = data.turns.get_mut(&id).unwrap();
                            let interrupted = turn.snapshot.status == TurnStatus::Interrupting;
                            (turn.request.take().unwrap(), turn_deadline, interrupted)
                        };
                        session.notify();
                        if session.data.lock().unwrap().status == SessionStatus::Closing {
                            terminate(
                                &session, &mut commands, &backend,
                                TurnStatus::Interrupted, false,
                            ).await;
                            break;
                        }
                        if interrupted {
                            if !backend.confirm_quiescent().await {
                                terminate(
                                    &session, &mut commands, &backend,
                                    TurnStatus::Interrupted, false,
                                ).await;
                                break;
                            }
                            session.data.lock().unwrap().active.as_mut().unwrap()
                                .completion_outcome = Some(TurnStatus::Interrupted);
                            if session.commit_completion(true).await.is_err() {
                                terminate(
                                    &session, &mut commands, &backend, TurnStatus::Unknown, false,
                                ).await;
                                break;
                            }
                            session.notify();
                            continue;
                        }
                        let execution = async {
                            if let Some(owner) = session.ledger.get() {
                                owner.dispatch(&id).await.map_err(|_| ())?;
                            }
                            if session.data.lock().unwrap().turns[&id].snapshot.status
                                == TurnStatus::Interrupting
                            {
                                return Ok(false);
                            }
                            backend.execute(id, request).await.map(|_| true)
                        };
                        let (executed, suspend) = tokio::select! {
                            _ = session.ownership_lost() => (None, false),
                            _ = session.daemon_stopping() => (None, true),
                            result = tokio::time::timeout_at(turn_deadline, execution) =>
                                (Some(result), false),
                        };
                        if matches!(executed, Some(Ok(Ok(false)))) {
                            if !backend.confirm_quiescent().await {
                                terminate(
                                    &session, &mut commands, &backend,
                                    TurnStatus::Interrupted, false,
                                ).await;
                                break;
                            }
                            session.data.lock().unwrap().active.as_mut().unwrap()
                                .completion_outcome = Some(TurnStatus::Interrupted);
                            if session.commit_completion(true).await.is_err() {
                                terminate(
                                    &session, &mut commands, &backend, TurnStatus::Unknown, false,
                                ).await;
                                break;
                            }
                        } else if !matches!(executed, Some(Ok(Ok(true)))) {
                            let outcome = if suspend { TurnStatus::Interrupted }
                                else if matches!(executed, Some(Err(_))) { TurnStatus::TimedOut }
                                else { TurnStatus::Unknown };
                            terminate(&session, &mut commands, &backend, outcome, suspend).await;
                            break;
                        }
                    }
                    Some(Command::Steer { turn_id, text, reply }) => {
                        let active = session.data.lock().unwrap().require_steerable(&turn_id);
                        let result = match active {
                            Ok(()) => tokio::select! {
                                _ = session.ownership_lost() => Err(ApiError::conflict(
                                    "ownership changed; reconcile steering delivery",
                                )),
                                _ = session.daemon_stopping() => Err(ApiError::conflict(
                                    "daemon stopping; reconcile steering delivery",
                                )),
                                result = tokio::time::timeout_at(
                                    deadline, backend.steer(turn_id, text),
                                ) => {
                                    result.unwrap_or(Err(ApiError::conflict(
                                        "steering deadline exceeded; reconcile delivery",
                                    )))
                                }
                            },
                            Err(error) => Err(error),
                        };
                        let _ = reply.send(result);
                    }
                    Some(Command::Notice { turn_id, change, reply }) => {
                        // The pending notice belongs to the turn that posted
                        // it: only that turn may withdraw it, so a late DELETE
                        // for an earlier turn cannot clear a newer turn's notice.
                        let result = match change {
                            NoticeChange::Post(notice) => {
                                session.data.lock().unwrap().require_steerable(&turn_id).map(|()| {
                                    let posted = backend.post_notice(notice);
                                    if posted {
                                        notice_owner = Some(turn_id);
                                    }
                                    posted
                                })
                            }
                            NoticeChange::Withdraw(id) => {
                                let withdrawn = notice_owner.as_deref() == Some(turn_id.as_str())
                                    && backend.withdraw_notice(id);
                                if withdrawn {
                                    notice_owner = None;
                                }
                                Ok(withdrawn)
                            }
                        };
                        let _ = reply.send(result);
                    }
                    Some(Command::Interrupt(id)) => {
                        // A queued interrupt cannot hit a later HTTP turn: that
                        // turn cannot be admitted before we consume its predecessor's
                        // matching terminal event.
                        let active = {
                            let data = session.data.lock().unwrap();
                            data.require_active(&id).is_ok() && data.turns[&id].request.is_none()
                        };
                        if active {
                            let interrupted = tokio::select! {
                                _ = session.daemon_stopping() => {
                                    terminate(
                                        &session, &mut commands, &backend,
                                        TurnStatus::Interrupted, session.ledger.get().is_some(),
                                    ).await;
                                    break;
                                }
                                _ = session.ownership_lost() => None,
                                result = tokio::time::timeout_at(deadline, backend.interrupt()) =>
                                    Some(result),
                            };
                            if !matches!(interrupted, Some(Ok(Ok(())))) {
                                terminate(
                                    &session, &mut commands, &backend, TurnStatus::Unknown, false,
                                ).await;
                                break;
                            }
                        }
                    }
                    command @ (Some(Command::Close) | Some(Command::Suspend) | None) => {
                        let suspend = matches!(command, Some(Command::Suspend));
                        terminate(
                            &session, &mut commands, &backend, TurnStatus::Interrupted, suspend,
                        ).await;
                        break;
                    }
                }
            }
            event = backend.next_event() => {
                match event {
                    Ok(event) => {
                        let shutdown = matches!(event.msg, EventMsg::ShutdownComplete);
                        if session.observe_committed(&event, backend.is_quiescent())
                            .await.is_err()
                        {
                            terminate(
                                &session, &mut commands, &backend, TurnStatus::Unknown, false,
                            ).await;
                            break;
                        }
                        let quiescent = backend.confirm_quiescent().await;
                        let autonomous = session.ledger.get().is_some()
                            && session.data.lock().unwrap().active.is_none()
                            && !quiescent;
                        if shutdown || autonomous
                            || session.commit_completion(quiescent).await.is_err()
                        {
                            terminate(
                                &session, &mut commands, &backend, TurnStatus::Unknown, false,
                            ).await;
                            break;
                        }
                    }
                    Err(()) => {
                        terminate(
                            &session, &mut commands, &backend, TurnStatus::Unknown, false,
                        ).await;
                        break;
                    }
                }
            }
            activity = backend.activity_changed() => {
                if activity.is_err() {
                    terminate(&session, &mut commands, &backend, TurnStatus::Unknown, false).await;
                    break;
                }
                let quiescent = backend.confirm_quiescent().await;
                let autonomous = session.ledger.get().is_some()
                    && session.data.lock().unwrap().active.is_none()
                    && !quiescent;
                if autonomous || session.commit_completion(quiescent).await.is_err() {
                    terminate(&session, &mut commands, &backend, TurnStatus::Unknown, false).await;
                    break;
                }
                session.notify();
            }
            // Descendant activity does not necessarily wake the root's watch.
            // Poll only while a root terminal outcome awaits whole-tree quiet.
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)),
                if session.data.lock().unwrap().active.as_ref()
                    .is_some_and(|active| active.completion_outcome.is_some()) => {
                if session.commit_completion(backend.confirm_quiescent().await).await.is_err() {
                    terminate(&session, &mut commands, &backend, TurnStatus::Unknown, false).await;
                    break;
                }
            }
        }
    }
}

async fn terminate<B: Backend>(
    session: &Session,
    commands: &mut mpsc::Receiver<Command>,
    backend: &B,
    outcome: TurnStatus,
    suspend: bool,
) {
    if !stop(session, backend, outcome, suspend).await {
        unknown_loop(session, commands, backend).await;
    }
}

async fn stop<B: Backend>(
    session: &Session,
    backend: &B,
    outcome: TurnStatus,
    suspend: bool,
) -> bool {
    session.data.lock().unwrap().status = SessionStatus::Closing;
    let stopped = matches!(
        tokio::time::timeout(CLEANUP_GRACE, backend.shutdown()).await,
        Ok(Ok(()))
    );
    let committed = if stopped {
        if let Some(owner) = session.ledger.get() {
            let active = {
                let data = session.data.lock().unwrap();
                data.active.as_ref().map(|active| {
                    let mut snapshot = data.turns[&active.id].snapshot.clone();
                    snapshot.status = outcome;
                    snapshot
                })
            };
            let commit = async {
                if let Some(snapshot) = active {
                    let snapshot = owner.finish(snapshot).await?;
                    let id = snapshot.turn_id.clone();
                    session
                        .data
                        .lock()
                        .unwrap()
                        .turns
                        .get_mut(&id)
                        .unwrap()
                        .snapshot = snapshot;
                    if outcome == TurnStatus::Unknown {
                        return Err(ApiError::conflict(
                            "unknown turn requires explicit resolution",
                        ));
                    }
                } else {
                    // Confirmed shutdown is independent containment of startup
                    // or autonomous work with no foreground turn.
                    owner.quarantine().await?;
                    owner.reconcile_idle().await?;
                }
                owner.retire(suspend).await
            }
            .await;
            commit.is_ok()
        } else {
            true
        }
    } else {
        if let Some(owner) = session.ledger.get() {
            let _ = owner.quarantine().await;
        }
        false
    };
    let mut data = session.data.lock().unwrap();
    if committed {
        if let Some(id) = data.active.as_ref().map(|active| active.id.clone()) {
            data.finish_turn(&id, outcome);
        }
        data.status = SessionStatus::Closed;
        data.orphan_permit = None;
    } else {
        data.mark_unknown();
    }
    drop(data);
    session.notify();
    session.done.send_replace(true);
    committed
}

async fn unknown_loop<B: Backend>(
    session: &Session,
    commands: &mut mpsc::Receiver<Command>,
    backend: &B,
) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::Close if stop(session, backend, TurnStatus::Interrupted, false).await => break,
            Command::Steer { reply, .. } => {
                let _ = reply.send(Err(ApiError::conflict("session outcome is unknown")));
            }
            Command::Notice { reply, .. } => {
                let _ = reply.send(Err(ApiError::conflict("session outcome is unknown")));
            }
            _ => {}
        }
    }
}
