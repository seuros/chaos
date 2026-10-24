//! Opt-in journal ownership and commit boundaries. No HTTP future owns a write
//! or an executor; losing a response is reconciliation, never redispatch.
mod invocations;
pub(crate) mod policy;
pub(crate) mod recovery;
mod replay;
use chaos_snitch::tracing;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chaos_journald::{
    ErrorCode, ExecutionLease, ExecutionRequest as Request, ExecutionResponse as Response,
    ExecutionSessionState, ExecutionTurn, ExecutionTurnState, JournalClient, JournalClientError,
};
use rama::http::StatusCode;
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::time::Instant;
use uuid::Uuid;

use super::{
    ActiveTurn, ApiError, Command, CreateRequest, Session, SessionManager, SessionStatus,
    TurnRecord, TurnRequest, TurnSnapshot, TurnStatus, bounded_text, fingerprint,
};

const LEASE_MS: u64 = 90_000;
const STORE_TIMEOUT: Duration = Duration::from_secs(5);
const HEARTBEAT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub(super) enum Client {
    Journal(JournalClient),
    #[cfg(test)]
    Sqlite(Arc<chaos_journald::SqliteJournalStore>),
}

pub(crate) struct Store {
    pub(super) client: Client,
    pub(crate) scope: String,
    pub(super) policy_digest: Option<String>,
    pub(super) policy: Option<Arc<policy::PinnedPolicy>>,
    pub(super) recovery_only: bool,
}

impl Store {
    pub(crate) fn billing_enabled(&self) -> bool {
        matches!(&self.client, Client::Journal(JournalClient::Postgres(_)))
    }

    async fn bind_scope(&self) -> Result<(), ApiError> {
        if self.recovery_only {
            return Ok(());
        }
        let digest = self.policy_digest.clone().ok_or_else(unavailable)?;
        match self
            .call(Request::BindScope {
                scope: self.scope.clone(),
                policy_digest: digest.clone(),
            })
            .await?
        {
            Response::Scope(Some(bound)) if bound == digest => Ok(()),
            _ => Err(unavailable()),
        }
    }

    fn check_execution(&self) -> Result<(), ApiError> {
        if self.recovery_only {
            return Err(ApiError::conflict("recovery-only mode cannot execute work"));
        }
        if let Some(policy) = &self.policy {
            policy.check()?;
        } else {
            #[cfg(not(test))]
            return Err(unavailable());
        }
        Ok(())
    }

    pub(crate) async fn call(&self, request: Request) -> Result<Response, ApiError> {
        let result = tokio::time::timeout(STORE_TIMEOUT, async {
            match &self.client {
                Client::Journal(client) => client.execution(request).await.map_err(store_error),
                #[cfg(test)]
                Client::Sqlite(store) => {
                    use chaos_journald::JournalStore;
                    store.execution(request).await.map_err(|error| {
                        tracing::warn!(%error, "HTTP execution ledger failed");
                        match error {
                            chaos_journald::JournalError::ExecutionNotFound => {
                                ApiError::new(StatusCode::NOT_FOUND, "turn not found")
                            }
                            chaos_journald::JournalError::ExecutionConflict(_) => {
                                ApiError::conflict("execution ledger conflict; reconcile state")
                            }
                            _ => unavailable(),
                        }
                    })
                }
            }
        })
        .await;
        result.unwrap_or_else(|_| Err(unavailable()))
    }
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "execution ledger unavailable; reconcile before retrying",
    )
}

fn store_error(error: JournalClientError) -> ApiError {
    tracing::warn!(%error, "HTTP execution ledger failed");
    match error {
        JournalClientError::Remote(error) if error.code == ErrorCode::NotFound => {
            ApiError::new(StatusCode::NOT_FOUND, "turn not found")
        }
        JournalClientError::Remote(error) if error.code == ErrorCode::ExecutionConflict => {
            ApiError::conflict("execution ledger conflict; reconcile state")
        }
        _ => unavailable(),
    }
}

pub(super) struct Owner {
    store: Arc<Store>,
    lease: Mutex<Option<ExecutionLease>>,
    id: Uuid,
    lost: watch::Sender<bool>,
}

struct AcceptanceGuard<'a> {
    session: &'a Session,
    committed: bool,
}

impl Drop for AcceptanceGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.session.data.lock().unwrap().mark_unknown();
            if let Some(owner) = self.session.ledger.get() {
                owner.abandon();
            }
            self.session.notify();
        }
    }
}

impl Owner {
    fn new(store: Arc<Store>, id: Uuid, lease: Option<ExecutionLease>) -> Arc<Self> {
        let (lost, _) = watch::channel(false);
        Arc::new(Self {
            store,
            lease: Mutex::new(lease),
            id,
            lost,
        })
    }

    pub(super) async fn claim(
        store: Arc<Store>,
        id: Uuid,
        owner_id: String,
    ) -> Result<(Arc<Self>, chaos_journald::ExecutionSession), ApiError> {
        store.bind_scope().await?;
        let request = match &store.policy_digest {
            Some(digest) => Request::ClaimBound {
                session_id: id,
                scope: store.scope.clone(),
                policy_digest: digest.clone(),
                owner_id,
                ttl_ms: LEASE_MS,
            },
            None if store.recovery_only => Request::Claim {
                session_id: id,
                scope: store.scope.clone(),
                owner_id,
                ttl_ms: LEASE_MS,
            },
            None => return Err(unavailable()),
        };
        match store.call(request).await? {
            Response::Claimed { session, lease } => {
                Ok((Self::new(store, id, Some(lease)), session))
            }
            _ => Err(unavailable()),
        }
    }

    fn lease(&self) -> Result<ExecutionLease, ApiError> {
        if *self.lost.borrow() {
            return Err(unavailable());
        }
        self.lease.lock().unwrap().clone().ok_or_else(unavailable)
    }

    pub(super) fn abandon(&self) {
        self.lost.send_replace(true);
    }

    pub(super) async fn renew(&self) -> Result<(), ApiError> {
        let response = self
            .write(Request::Heartbeat {
                lease: self.lease()?,
                ttl_ms: LEASE_MS,
            })
            .await?;
        if let Response::Lease(lease) = response {
            let mut current = self.lease.lock().unwrap();
            // A delayed heartbeat response must not resurrect retired ownership.
            if current.is_some() {
                *current = Some(lease);
            }
            Ok(())
        } else {
            self.abandon();
            Err(unavailable())
        }
    }

    async fn write(&self, request: Request) -> Result<Response, ApiError> {
        let result = self.store.call(request).await;
        if result.is_err() {
            self.abandon();
        }
        result
    }

    pub(super) async fn quarantine(&self) -> Result<(), ApiError> {
        self.write(Request::Quarantine {
            lease: self.lease()?,
        })
        .await?;
        Ok(())
    }

    pub(super) async fn bind(&self, id: chaos_ipc::ProcessId) -> Result<(), ApiError> {
        self.write(Request::BindProcess {
            lease: self.lease()?,
            process_id: id,
        })
        .await?;
        Ok(())
    }

    pub(super) async fn bind_new(&self, id: chaos_ipc::ProcessId) -> Result<(), ApiError> {
        self.write(Request::BindNewProcess {
            lease: self.lease()?,
            process_id: id,
        })
        .await?;
        Ok(())
    }

    pub(super) fn check_execution(&self) -> Result<(), ApiError> {
        let result = self.store.check_execution();
        if result.is_err() {
            self.abandon();
        }
        result
    }

    pub(super) async fn reconcile_idle(&self) -> Result<(), ApiError> {
        self.write(Request::ReconcileIdle {
            lease: self.lease()?,
        })
        .await?;
        Ok(())
    }

    pub(super) async fn dispatch(&self, id: &str) -> Result<(), ApiError> {
        self.check_execution()?;
        let response = self
            .write(Request::Dispatch {
                lease: self.lease()?,
                turn_id: parse_id(id)?,
            })
            .await?;
        if matches!(
            response,
            Response::Dispatched {
                newly_dispatched: true,
                ..
            }
        ) {
            Ok(())
        } else {
            self.abandon();
            Err(unavailable())
        }
    }

    pub(super) async fn finish(&self, snapshot: TurnSnapshot) -> Result<TurnSnapshot, ApiError> {
        let state = persisted_state(snapshot.status)?;
        let snapshot = bounded_snapshot(snapshot)?;
        self.write(Request::Finish {
            lease: self.lease()?,
            turn_id: parse_id(&snapshot.turn_id)?,
            state,
            outcome: Some(serde_json::to_value(&snapshot).unwrap()),
        })
        .await?;
        Ok(snapshot)
    }

    pub(super) async fn retire(&self, suspend: bool) -> Result<(), ApiError> {
        if !suspend {
            self.write(Request::Close {
                lease: self.lease()?,
            })
            .await?;
        }
        self.write(Request::Release {
            lease: self.lease()?,
        })
        .await?;
        *self.lease.lock().unwrap() = None;
        Ok(())
    }

    async fn get_turn(&self, id: &str) -> Result<ExecutionTurn, ApiError> {
        match self
            .store
            .call(Request::GetTurn {
                session_id: self.id,
                turn_id: parse_id(id)?,
            })
            .await?
        {
            Response::Turn(turn) => Ok(turn),
            _ => Err(unavailable()),
        }
    }

    async fn check_turn_capacity(&self) -> Result<(), ApiError> {
        match self
            .store
            .call(Request::GetSession {
                session_id: self.id,
            })
            .await?
        {
            Response::Session(record) if record.turn_count >= super::MAX_TURNS as u64 => Err(
                ApiError::capacity("turn limit reached; create a new session"),
            ),
            Response::Session(_) => Ok(()),
            _ => Err(unavailable()),
        }
    }
}

fn bounded_snapshot(mut snapshot: TurnSnapshot) -> Result<TurnSnapshot, ApiError> {
    // JSON escaping can make a 16 KiB text exceed the ledger's 32 KiB bound.
    while serde_json::to_vec(&snapshot).unwrap().len() > chaos_journald::MAX_EXECUTION_OUTCOME_BYTES
    {
        let Some(text) = &snapshot.result else {
            return Err(unavailable());
        };
        if text.is_empty() {
            return Err(unavailable());
        }
        snapshot.result = Some(bounded_text(text, text.len() / 2).0);
        snapshot.result_truncated = true;
    }
    Ok(snapshot)
}

fn parse_id(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| ApiError::bad_request("expected a UUID"))
}

fn persisted_state(status: TurnStatus) -> Result<ExecutionTurnState, ApiError> {
    Ok(match status {
        TurnStatus::Completed => ExecutionTurnState::Completed,
        TurnStatus::Failed => ExecutionTurnState::Failed,
        TurnStatus::Interrupted => ExecutionTurnState::Interrupted,
        TurnStatus::TimedOut => ExecutionTurnState::TimedOut,
        TurnStatus::Unknown => ExecutionTurnState::Unknown,
        _ => return Err(unavailable()),
    })
}

fn snapshot(turn: &ExecutionTurn) -> Result<TurnSnapshot, ApiError> {
    let status = match turn.state {
        ExecutionTurnState::Accepted
        | ExecutionTurnState::Running
        | ExecutionTurnState::Unknown => {
            // This function is used only for records with no live local turn.
            TurnStatus::Unknown
        }
        ExecutionTurnState::Completed => TurnStatus::Completed,
        ExecutionTurnState::Failed => TurnStatus::Failed,
        ExecutionTurnState::Interrupted => TurnStatus::Interrupted,
        ExecutionTurnState::TimedOut => TurnStatus::TimedOut,
    };
    let mut snapshot = if let Some(outcome) = &turn.outcome {
        serde_json::from_value::<TurnSnapshot>(outcome.clone()).map_err(|_| unavailable())?
    } else {
        TurnSnapshot {
            turn_id: turn.turn_id.to_string(),
            status,
            result: None,
            result_truncated: false,
            error: None,
            usage: None,
        }
    };
    if snapshot.turn_id != turn.turn_id.to_string() {
        return Err(unavailable());
    }
    snapshot.status = status;
    Ok(snapshot)
}

pub(crate) type Recovered = (Arc<Session>, mpsc::Receiver<Command>, CreateRequest);

impl SessionManager {
    pub(crate) fn recovery_only(&self) -> bool {
        self.store.as_ref().is_some_and(|store| store.recovery_only)
    }

    pub(crate) fn check_execution(&self) -> Result<(), ApiError> {
        match &self.store {
            Some(store) => store.check_execution(),
            None => Ok(()),
        }
    }

    pub(crate) fn pinned_auth(&self) -> Option<Arc<chaos_kern::AuthManager>> {
        self.store
            .as_ref()?
            .policy
            .as_ref()
            .map(|policy| policy.auth())
    }
    pub(crate) fn pinned_config(&self) -> Option<Arc<chaos_kern::config::Config>> {
        self.store
            .as_ref()?
            .policy
            .as_ref()
            .map(|policy| policy.config())
    }

    pub(crate) async fn persistent(
        limit: usize,
        ttl: Duration,
        client: JournalClient,
        scope: String,
        capacity: &Arc<Semaphore>,
        policy: Option<Arc<policy::PinnedPolicy>>,
        recovery_only: bool,
    ) -> Result<(Self, Vec<Recovered>), ApiError> {
        let mut store = Store {
            client: Client::Journal(client),
            scope,
            policy_digest: policy.as_ref().map(|pin| pin.digest().to_owned()),
            policy,
            recovery_only,
        };
        if recovery_only {
            match store
                .call(Request::GetScope {
                    scope: store.scope.clone(),
                })
                .await?
            {
                Response::Scope(digest) => store.policy_digest = digest,
                _ => return Err(unavailable()),
            }
        }
        Self::restore(limit, ttl, Arc::new(store), capacity).await
    }

    pub(super) async fn restore(
        limit: usize,
        ttl: Duration,
        store: Arc<Store>,
        capacity: &Arc<Semaphore>,
    ) -> Result<(Self, Vec<Recovered>), ApiError> {
        store.bind_scope().await?;
        let mut manager = Self::new(limit, ttl);
        manager.store = Some(store.clone());
        let mut after = None;
        let mut recovered = Vec::new();
        loop {
            let Response::Sessions(page) = store
                .call(Request::ListSessions {
                    scope: store.scope.clone(),
                    after,
                    limit: 256,
                })
                .await?
            else {
                return Err(unavailable());
            };
            if page.is_empty() {
                break;
            }
            after = page.last().map(|record| record.session_id);
            for record in page {
                let (session, commands) =
                    manager.reserve_id(record.process_id, record.session_id)?;
                let (owner, mut record) = if record.state == ExecutionSessionState::Closed {
                    (Owner::new(store.clone(), record.session_id, None), record)
                } else {
                    Owner::claim(store.clone(), record.session_id, manager.daemon_id.clone())
                        .await?
                };
                // Boot requires a committed quarantine. An idle, unbound
                // record cannot authorize manufacturing fresh history here.
                if record.state == ExecutionSessionState::Idle && record.process_id.is_none() {
                    owner.retire(false).await?;
                    record.state = ExecutionSessionState::Closed;
                }
                if store.recovery_only && record.state == ExecutionSessionState::Idle {
                    owner.quarantine().await?;
                    record.state = ExecutionSessionState::Unknown;
                }
                session.ledger.set(owner).map_err(|_| unavailable())?;
                session.restore_replay().await?;
                {
                    let mut data = session.data.lock().unwrap();
                    data.status = match record.state {
                        ExecutionSessionState::Closed => SessionStatus::Closed,
                        ExecutionSessionState::Idle => SessionStatus::Idle,
                        ExecutionSessionState::Active | ExecutionSessionState::Unknown => {
                            SessionStatus::Unknown
                        }
                    };
                    if data.status == SessionStatus::Unknown {
                        let permit = capacity.clone().try_acquire_owned().map_err(|_| {
                            ApiError::capacity(
                                "uncontained recovered work exceeds execution capacity",
                            )
                        })?;
                        if let Some(turn_id) = record.active_turn_id {
                            data.active = Some(ActiveTurn {
                                id: turn_id.to_string(),
                                deadline: Instant::now(),
                                completion_outcome: None,
                                _permit: permit,
                            });
                        } else {
                            data.orphan_permit = Some(permit);
                        }
                    }
                }
                if let Some(id) = record.active_turn_id {
                    session.load_turn(&id.to_string()).await?;
                }
                if record.state == ExecutionSessionState::Idle {
                    // A recovered process is started only on a fresh accepted
                    // turn. No old prompt is ever loaded for dispatch.
                    recovered.push((
                        session,
                        commands,
                        CreateRequest {
                            session_id: Some(record.session_id),
                            resume_process_id: record.process_id,
                        },
                    ));
                }
            }
        }
        for session in manager.sessions.lock().unwrap().values() {
            session.heartbeat();
        }
        Ok((manager, recovered))
    }

    pub(super) async fn create(
        self: &Arc<Self>,
        state: Arc<crate::ServerState>,
        request: CreateRequest,
    ) -> Result<Arc<Session>, ApiError> {
        let _creation = self.creation.lock().await;
        self.ensure_running()?;
        if let Some(store) = &self.store {
            store.check_execution()?;
        }
        if let Some(id) = request.session_id
            && let Ok(previous) = self.get(&id.to_string())
        {
            if request.resume_process_id.is_some_and(|id| {
                previous.data.lock().unwrap().process_id.as_deref() != Some(&id.to_string())
            }) {
                return Err(ApiError::conflict(
                    "session id reused with a different process",
                ));
            }
            return Ok(previous);
        }
        if let Some(id) = request.resume_process_id
            && state.process_table.get_process(id).await.is_ok()
        {
            return Err(ApiError::conflict("process is already running"));
        }
        let startup_permit =
            if self.store.is_some() {
                Some(state.semaphore.clone().try_acquire_owned().map_err(|_| {
                    ApiError::capacity("execution capacity exhausted during startup")
                })?)
            } else {
                None
            };
        let (session, commands) = self.reserve_id(
            request.resume_process_id,
            request.session_id.unwrap_or_else(Uuid::new_v4),
        )?;
        session.data.lock().unwrap().orphan_permit = startup_permit;
        if let Some(store) = &self.store {
            // Reserve memory first, then commit creation before starting any
            // kernel. An ambiguous claim remains a non-executable local record.
            let claim = Owner::claim(
                store.clone(),
                parse_id(&session.id)?,
                self.daemon_id.clone(),
            )
            .await;
            let (owner, record) = match claim {
                Ok(claim) => claim,
                Err(error) => {
                    session.data.lock().unwrap().status = SessionStatus::Unknown;
                    return Err(error);
                }
            };
            session.ledger.set(owner).map_err(|_| unavailable())?;
            if record.process_id.is_some() || record.state != ExecutionSessionState::Idle {
                session.data.lock().unwrap().status = SessionStatus::Unknown;
                return Err(ApiError::conflict(
                    "persisted session requires restart reconciliation",
                ));
            }
            if let Some(id) = request.resume_process_id
                && let Err(error) = session.ledger.get().unwrap().bind(id).await
            {
                session.data.lock().unwrap().mark_unknown();
                session.notify();
                return Err(error);
            }
            session.heartbeat();
        }
        super::runtime::spawn(state, session.clone(), commands, request, false);
        Ok(session)
    }
}

impl Session {
    pub(super) fn heartbeat(self: &Arc<Self>) {
        let Some(owner) = self.ledger.get().cloned() else {
            return;
        };
        if owner.lease.lock().unwrap().is_none() {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(HEARTBEAT).await;
                let Some(session) = weak.upgrade() else { break };
                if owner.lease.lock().unwrap().is_none() || *owner.lost.borrow() {
                    break;
                }
                let result = owner.renew().await;
                let result = result.and_then(|()| {
                    if owner.store.recovery_only {
                        Ok(())
                    } else {
                        owner.check_execution()
                    }
                });
                if result.is_err() {
                    if owner.lease.lock().unwrap().is_none() {
                        break;
                    }
                    owner.abandon();
                    let mut data = session.data.lock().unwrap();
                    if !matches!(data.status, SessionStatus::Closed | SessionStatus::Failed) {
                        data.mark_unknown();
                    }
                    drop(data);
                    session.notify();
                    break;
                }
            }
        });
    }

    pub(super) async fn ownership_lost(&self) {
        let Some(owner) = self.ledger.get() else {
            std::future::pending::<()>().await;
            return;
        };
        let mut lost = owner.lost.subscribe();
        while !*lost.borrow_and_update() {
            if lost.changed().await.is_err() {
                break;
            }
        }
    }

    pub(super) async fn daemon_stopping(&self) {
        let mut stopping = self.stopping.clone();
        while !*stopping.borrow_and_update() {
            if stopping.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    async fn load_turn(&self, id: &str) -> Result<TurnSnapshot, ApiError> {
        let owner = self.ledger.get().ok_or_else(unavailable)?;
        let turn = owner.get_turn(id).await?;
        let snapshot = snapshot(&turn)?;
        let mut data = self.data.lock().unwrap();
        let record = data.turns.entry(id.into()).or_insert_with(|| TurnRecord {
            fingerprint: turn.fingerprint,
            request: None,
            snapshot: snapshot.clone(),
        });
        Ok(record.snapshot.clone())
    }

    pub(super) async fn read_turn(&self, id: &str) -> Result<TurnSnapshot, ApiError> {
        match self.turn(id) {
            Ok(turn) => Ok(turn),
            Err(error) if error.code == StatusCode::NOT_FOUND && self.ledger.get().is_some() => {
                self.load_turn(id).await
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn admit(
        &self,
        id: String,
        request: TurnRequest,
        capacity: &Arc<Semaphore>,
        timeout: Duration,
    ) -> Result<TurnSnapshot, ApiError> {
        let Some(owner) = self.ledger.get() else {
            return self.submit(id, request, capacity, timeout);
        };
        owner.check_execution()?;
        request.validate()?;
        let fingerprint = fingerprint(&request);
        let _admission = self.admission.lock().await;
        self.notify();
        match self.read_turn(&id).await {
            Ok(previous) => {
                if self.data.lock().unwrap().turns[&id].fingerprint != fingerprint {
                    return Err(ApiError::conflict("turn id reused with different payload"));
                }
                return Ok(previous);
            }
            Err(error) if error.code == StatusCode::NOT_FOUND => {}
            Err(error) => return Err(error),
        }
        if self.data.lock().unwrap().status != SessionStatus::Idle {
            return Err(ApiError::conflict("session is not idle"));
        }
        owner.check_turn_capacity().await?;
        let slot = self
            .commands
            .try_reserve()
            .map_err(|_| ApiError::capacity("session command queue unavailable"))?;
        let snapshot = TurnSnapshot {
            turn_id: id.clone(),
            status: TurnStatus::Persisting,
            result: None,
            result_truncated: false,
            error: None,
            usage: None,
        };
        {
            let mut data = self.data.lock().unwrap();
            if data.status != SessionStatus::Idle || data.active.is_some() {
                return Err(ApiError::conflict("session is not idle"));
            }
            let permit = capacity
                .clone()
                .try_acquire_owned()
                .map_err(|_| ApiError::capacity("execution capacity exhausted"))?;
            data.active = Some(ActiveTurn {
                id: id.clone(),
                deadline: Instant::now() + timeout,
                completion_outcome: None,
                _permit: permit,
            });
            data.turns.insert(
                id.clone(),
                TurnRecord {
                    fingerprint: fingerprint.clone(),
                    request: Some(request),
                    snapshot: snapshot.clone(),
                },
            );
            data.status = SessionStatus::Running;
        }
        self.notify();
        let mut acceptance = AcceptanceGuard {
            session: self,
            committed: false,
        };
        // The caller is a detached daemon task. A disconnected HTTP client
        // cannot cancel this write or orphan a committed acceptance.
        let accepted = async {
            owner
                .write(Request::AcceptMetered {
                    lease: owner.lease()?,
                    turn_id: parse_id(&id)?,
                    fingerprint,
                })
                .await
        }
        .await;
        match accepted {
            Ok(Response::Accepted {
                newly_accepted: true,
                ..
            }) => {
                let snapshot = {
                    let mut data = self.data.lock().unwrap();
                    let snapshot = &mut data.turns.get_mut(&id).unwrap().snapshot;
                    if snapshot.status == TurnStatus::Persisting {
                        snapshot.status = TurnStatus::Accepted;
                    }
                    let snapshot = snapshot.clone();
                    // Publish steerability only after Turn is queued. A
                    // recovered lazy supervisor must see Turn before controls.
                    slot.send(Command::Turn(id));
                    snapshot
                };
                acceptance.committed = true;
                self.notify();
                Ok(snapshot)
            }
            _ => {
                self.data.lock().unwrap().mark_unknown();
                self.data
                    .lock()
                    .unwrap()
                    .turns
                    .get_mut(&id)
                    .unwrap()
                    .request = None;
                self.notify();
                Err(unavailable())
            }
        }
    }

    pub(super) async fn commit_completion(&self, quiescent: bool) -> Result<(), ApiError> {
        if self.ledger.get().is_none() {
            self.data.lock().unwrap().maybe_finish(quiescent);
            return Ok(());
        }
        if !quiescent {
            return Ok(());
        }
        let completion = {
            let data = self.data.lock().unwrap();
            data.active.as_ref().and_then(|active| {
                active.completion_outcome.map(|status| {
                    let mut snapshot = data.turns[&active.id].snapshot.clone();
                    snapshot.status = status;
                    snapshot
                })
            })
        };
        if let Some(snapshot) = completion {
            let snapshot = self.ledger.get().unwrap().finish(snapshot).await?;
            let mut data = self.data.lock().unwrap();
            data.turns.get_mut(&snapshot.turn_id).unwrap().snapshot = snapshot.clone();
            data.finish_turn(&snapshot.turn_id, snapshot.status);
            drop(data);
            self.notify();
        }
        Ok(())
    }
}
