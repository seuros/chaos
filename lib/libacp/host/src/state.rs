use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use protocol::schema::v1::{
    ClientCapabilities, ElicitationId, PermissionOption, PermissionOptionKind,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, SessionId,
    SessionNotification, SessionUpdate, ToolCallUpdate,
};
use protocol::{Client, ConnectionTo};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::lifecycle::{Connection, Session};
use crate::{AgentSession, ConnectionLimits, protocol};

mod connection;
mod turn;

pub(crate) use turn::{Turn, TurnGuard};

pub(crate) fn error(message: &'static str) -> protocol::Error {
    protocol::Error::internal_error().data(message)
}

pub(crate) fn invalid(message: &'static str) -> protocol::Error {
    protocol::Error::invalid_params().data(message)
}

pub(crate) struct State {
    pub client_capabilities: OnceLock<ClientCapabilities>,
    pub limits: ConnectionLimits,
    pub stop: CancellationToken,
    pub peer_disconnected: AtomicBool,
    pub sessions: Mutex<HashMap<SessionId, Arc<Slot>>>,
    pub session_permits: Arc<Semaphore>,
    pub requests: Arc<Semaphore>,
    pub inbound: AtomicUsize,
    pub url_elicitations: Mutex<HashMap<ElicitationId, UrlElicitation>>,
    connection: Mutex<Connection>,
    cleanup_done: CancellationToken,
}

impl State {
    pub fn new(limits: ConnectionLimits) -> Arc<Self> {
        Arc::new(Self {
            client_capabilities: OnceLock::new(),
            session_permits: Arc::new(Semaphore::new(limits.max_sessions)),
            requests: Arc::new(Semaphore::new(limits.pending_requests)),
            limits,
            stop: CancellationToken::new(),
            peer_disconnected: AtomicBool::new(false),
            sessions: Mutex::new(HashMap::new()),
            inbound: AtomicUsize::new(0),
            url_elicitations: Mutex::new(HashMap::new()),
            connection: Mutex::new(Connection::default()),
            cleanup_done: CancellationToken::new(),
        })
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
}

pub(crate) struct UrlElicitation {
    pub slot: Weak<Slot>,
    pub ticket: Arc<()>,
}

pub(crate) struct Slot {
    pub native: Arc<dyn AgentSession>,
    pub queued: AtomicUsize,
    pub queue_changed: tokio::sync::Notify,
    pub released: tokio::sync::Notify,
    pub shutdown: tokio::sync::Mutex<Option<bool>>,
    pub _permit: OwnedSemaphorePermit,
    lifecycle: Mutex<Session>,
}

impl Slot {
    pub fn opening(
        native: Arc<dyn AgentSession>,
        turn: Arc<Turn>,
        permit: OwnedSemaphorePermit,
    ) -> Arc<Self> {
        Arc::new(Self {
            native,
            lifecycle: Mutex::new(Session::opening(turn)),
            queued: AtomicUsize::new(0),
            queue_changed: tokio::sync::Notify::new(),
            released: tokio::sync::Notify::new(),
            shutdown: tokio::sync::Mutex::new(None),
            _permit: permit,
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, Session> {
        self.lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn invalidate(&self) {
        self.lock().invalidate();
    }

    pub fn is_invalid(&self) -> bool {
        self.lock().is_invalid()
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
    /// Capabilities negotiated on this connection, never inferred from a reply.
    pub fn capabilities(&self) -> protocol::Result<ClientCapabilities> {
        let (state, _) = self.ready()?;
        Ok(state.client_capabilities.get().cloned().unwrap_or_default())
    }

    /// Replay an update with bounded backpressure instead of buffering a whole history.
    pub async fn update_wait(&self, update: SessionUpdate) -> protocol::Result<()> {
        let (state, slot) = self.ready()?;
        let Some(update) = state.filter_update(update) else {
            return Ok(());
        };
        loop {
            let changed = slot.queue_changed.notified();
            {
                let active = slot.lock();
                self.check_ready(&state, &active)?;
                if slot.queued.load(Ordering::Acquire) < state.limits.update_capacity {
                    slot.queued.fetch_add(1, Ordering::AcqRel);
                    return self.connection.send_notification(SessionNotification::new(
                        self.session_id.clone(),
                        update,
                    ));
                }
            }
            tokio::select! {
                _ = changed => {},
                _ = state.stop.cancelled() => return Err(error("ACP connection closed")),
                _ = self.turn.cancel.cancelled() => return Err(error("ACP operation cancelled")),
            }
        }
    }

    pub(crate) fn ready(&self) -> protocol::Result<(Arc<State>, Arc<Slot>)> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| error("ACP connection closed"))?;
        let slot = self
            .slot
            .upgrade()
            .ok_or_else(|| error("ACP session closed"))?;
        self.check_ready(&state, &slot.lock())?;
        Ok((state, slot))
    }

    pub(crate) fn session_ready(&self) -> protocol::Result<(Arc<State>, Arc<Slot>)> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| error("ACP connection closed"))?;
        let slot = state.find(&self.session_id)?;
        if !self
            .slot
            .upgrade()
            .is_some_and(|owner| Arc::ptr_eq(&owner, &slot))
        {
            return Err(error("ACP session closed"));
        }
        slot.lock().check_open()?;
        Ok((state, slot))
    }

    pub(crate) fn check_ready(&self, state: &State, session: &Session) -> protocol::Result<()> {
        state.ready()?;
        session.check(&self.turn)
    }

    /// Queue an ordered update. Overflow fails the connection closed.
    pub fn update(&self, update: SessionUpdate) -> protocol::Result<()> {
        let (state, slot) = self.ready()?;
        let Some(update) = state.filter_update(update) else {
            return Ok(());
        };
        let active = slot.lock();
        self.check_ready(&state, &active)?;
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
        let (state, _) = self.ready()?;
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
        let request = RequestPermissionRequest::new(self.session_id.clone(), tool_call, options);
        match self
            .client_request_with_timeout(request, false, state.limits.approval_timeout)
            .await
        {
            Ok(reply) => match &reply.outcome {
                RequestPermissionOutcome::Selected(selected)
                    if ids.contains(&selected.option_id) =>
                {
                    Ok(reply)
                }
                _ => Ok(deny()),
            },
            _ => Ok(deny()),
        }
    }
}

pub(crate) struct ServeGuard(pub Arc<State>);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        self.0.close();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let state = self.0.clone();
            runtime.spawn(async move {
                let _ = state.cleanup().await;
            });
        }
    }
}

#[cfg(test)]
mod tests;
