use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use protocol::schema::v1::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionId, SessionNotification, SessionUpdate, ToolCallUpdate,
};
use protocol::{Client, ConnectionTo};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{AgentSession, ConnectionLimits, HostError, protocol};

pub(crate) fn error(message: &'static str) -> protocol::Error {
    protocol::Error::internal_error().data(message)
}

pub(crate) fn invalid(message: &'static str) -> protocol::Error {
    protocol::Error::invalid_params().data(message)
}

pub(crate) struct State {
    pub limits: ConnectionLimits,
    pub initialized: AtomicBool,
    pub stop: CancellationToken,
    pub sessions: Mutex<HashMap<SessionId, Arc<Slot>>>,
    pub session_permits: Arc<Semaphore>,
    pub requests: Arc<Semaphore>,
    pub inbound: AtomicUsize,
    pub failure: Mutex<Option<&'static str>>,
    cleanup_started: AtomicBool,
}

impl State {
    pub fn new(limits: ConnectionLimits) -> Arc<Self> {
        Arc::new(Self {
            session_permits: Arc::new(Semaphore::new(limits.max_sessions)),
            requests: Arc::new(Semaphore::new(limits.pending_requests)),
            limits,
            initialized: AtomicBool::new(false),
            stop: CancellationToken::new(),
            sessions: Mutex::new(HashMap::new()),
            inbound: AtomicUsize::new(0),
            failure: Mutex::new(None),
            cleanup_started: AtomicBool::new(false),
        })
    }

    pub fn fail(&self, reason: &'static str) {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(reason);
        self.stop.cancel();
    }

    pub fn ready(&self) -> protocol::Result<()> {
        if self.stop.is_cancelled() || !self.initialized.load(Ordering::Acquire) {
            Err(invalid("ACP connection must be initialized and open"))
        } else {
            Ok(())
        }
    }

    pub fn find(&self, id: &SessionId) -> protocol::Result<Arc<Slot>> {
        self.ready()?;
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| invalid("unknown ACP session"))
    }

    pub fn acknowledge_inbound(&self) {
        let _ = self
            .inbound
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(1))
            });
    }

    pub async fn cleanup(&self) -> Result<(), HostError> {
        if self.cleanup_started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.stop.cancel();
        let sessions =
            std::mem::take(&mut *self.sessions.lock().unwrap_or_else(PoisonError::into_inner));
        for slot in sessions.values() {
            if let Some(turn) = slot.lock_turn().as_ref() {
                turn.cancel.cancel();
            }
        }
        let cleanup =
            futures::future::join_all(sessions.values().map(|slot| slot.native.shutdown()));
        match tokio::time::timeout(self.limits.shutdown_timeout, cleanup).await {
            Ok(results) if results.iter().all(Result::is_ok) => Ok(()),
            _ => Err(HostError::ShutdownUnconfirmed),
        }
    }
}

pub(crate) struct Slot {
    pub native: Arc<dyn AgentSession>,
    pub turn: Mutex<Option<Arc<Turn>>>,
    pub queued: AtomicUsize,
    pub invalid: AtomicBool,
    pub _permit: OwnedSemaphorePermit,
}

impl Slot {
    pub fn lock_turn(&self) -> MutexGuard<'_, Option<Arc<Turn>>> {
        self.turn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) struct Turn {
    pub cancel: CancellationToken,
    pub live: AtomicBool,
    pub cancel_started: AtomicBool,
    pub cancel_done: CancellationToken,
}

impl Turn {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancel: CancellationToken::new(),
            live: AtomicBool::new(true),
            cancel_started: AtomicBool::new(false),
            cancel_done: CancellationToken::new(),
        })
    }
}

/// Turn-scoped identity and native cancellation signal.
#[derive(Clone)]
pub struct SessionContext {
    pub(crate) client: ClientHandle,
}

impl SessionContext {
    /// Opaque identifier owned by this connection.
    pub fn session_id(&self) -> &SessionId {
        &self.client.session_id
    }

    /// Typed access to this turn's editor connection.
    pub fn client(&self) -> &ClientHandle {
        &self.client
    }

    /// Whether interruption has been requested, not whether native work stopped.
    pub fn is_cancelled(&self) -> bool {
        self.client.turn.cancel.is_cancelled()
            || self
                .client
                .state
                .upgrade()
                .is_none_or(|state| state.stop.is_cancelled())
    }

    /// Resolves when cancellation or connection shutdown has been requested.
    pub async fn cancelled(&self) {
        let Some(state) = self.client.state.upgrade() else {
            return;
        };
        let stop = state.stop.clone();
        drop(state);
        tokio::select! {
            _ = self.client.turn.cancel.cancelled() => {},
            _ = stop.cancelled() => {},
        }
    }
}

/// Bounded, session/turn-scoped access to an ACP client.
#[derive(Clone)]
pub struct ClientHandle {
    pub(crate) session_id: SessionId,
    pub(crate) connection: ConnectionTo<Client>,
    pub(crate) state: Weak<State>,
    pub(crate) slot: Weak<Slot>,
    pub(crate) turn: Arc<Turn>,
}

impl ClientHandle {
    fn ready(&self) -> protocol::Result<(Arc<State>, Arc<Slot>)> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| error("ACP connection closed"))?;
        let slot = self
            .slot
            .upgrade()
            .ok_or_else(|| error("ACP session closed"))?;
        state.ready()?;
        if !self.turn.live.load(Ordering::Acquire) || slot.invalid.load(Ordering::Acquire) {
            return Err(error("ACP prompt is no longer active"));
        }
        Ok((state, slot))
    }

    /// Queue an ordered update. Overflow fails the connection closed.
    pub fn update(&self, update: SessionUpdate) -> protocol::Result<()> {
        let (state, slot) = self.ready()?;
        let _active = slot.lock_turn();
        self.ready()?;
        if slot.queued.fetch_add(1, Ordering::AcqRel) >= state.limits.update_capacity {
            state.fail("ACP update queue overflow");
            return Err(error("ACP update queue overflow"));
        }
        self.connection
            .send_notification(SessionNotification::new(self.session_id.clone(), update))
    }

    /// Ask for a once-only permission. Invalid, stale or cancelled replies deny.
    ///
    /// No selection bypasses the backend's native permission ceiling.
    pub async fn request_permission(
        &self,
        tool_call: ToolCallUpdate,
        options: Vec<PermissionOption>,
    ) -> protocol::Result<RequestPermissionResponse> {
        let (state, slot) = self.ready()?;
        let deny = || RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled);
        if self.turn.cancel.is_cancelled() {
            return Ok(deny());
        }
        let mut ids = std::collections::HashSet::new();
        if options.is_empty()
            || options.iter().any(|option| {
                !matches!(
                    option.kind,
                    PermissionOptionKind::AllowOnce | PermissionOptionKind::RejectOnce
                ) || !ids.insert(option.option_id.clone())
            })
        {
            return Err(invalid(
                "permission options must be unique once-only decisions",
            ));
        }
        let permit = state.requests.clone().try_acquire_owned().map_err(|_| {
            state.fail("ACP client request limit exceeded");
            error("ACP client request limit exceeded")
        })?;
        let request = RequestPermissionRequest::new(self.session_id.clone(), tool_call, options);
        let (raw_tx, raw_rx) = tokio::sync::oneshot::channel();
        let received = state.clone();
        {
            let _active = slot.lock_turn();
            self.ready()?;
            self.connection
                .prepare_request(request)
                .on_receiving_result(async move |reply| {
                    received.acknowledge_inbound();
                    let _ = raw_tx.send(reply);
                    Ok(())
                })?;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let watching = state.clone();
        self.connection.spawn(async move {
            // Retain the permit even if the prompt is cancelled. An unanswered
            // editor request remains accounted for until its reply or deadline.
            let _permit = permit;
            match tokio::time::timeout(watching.limits.approval_timeout, raw_rx).await {
                Ok(reply) => {
                    let _ = tx.send(reply);
                }
                Err(_) => watching.fail("ACP permission response timed out"),
            }
            Ok(())
        })?;
        let result = tokio::select! {
            _ = self.turn.cancel.cancelled() => return Ok(deny()),
            _ = state.stop.cancelled() => return Ok(deny()),
            response = rx => response,
        };
        match result {
            Ok(Ok(Ok(reply))) if self.ready().is_ok() && !self.turn.cancel.is_cancelled() => {
                match &reply.outcome {
                    RequestPermissionOutcome::Selected(selected)
                        if ids.contains(&selected.option_id) =>
                    {
                        Ok(reply)
                    }
                    _ => Ok(deny()),
                }
            }
            _ => Ok(deny()),
        }
    }
}

pub(crate) struct TurnGuard {
    pub slot: Arc<Slot>,
    pub turn: Arc<Turn>,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.turn.live.store(false, Ordering::Release);
        self.turn.cancel.cancel();
        self.slot.lock_turn().take();
    }
}

pub(crate) struct ServeGuard(pub Arc<State>);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        self.0.stop.cancel();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let state = self.0.clone();
            runtime.spawn(async move {
                let _ = state.cleanup().await;
            });
        }
    }
}
