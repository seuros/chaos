//! Mutable state belongs to an SDK session, never to a transport connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use chaos_kern::config::Config;
use chaos_kern::models_manager::CollaborationModesConfig;
use chaos_kern::{AuthManager, ProcessTable, WorkflowOwner};
use mcp_host::prelude::{RequestId, Server, Session, ToolError};
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::outgoing_message::UrlElicitations;

pub(crate) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct McpSessionState {
    sdk_session: Session,
    pub(crate) process_table: Arc<ProcessTable>,
    /// Workflow workers must never share the conversation's event consumers.
    pub(crate) workflow_process_table: Arc<ProcessTable>,
    pub(crate) workflow_owner: WorkflowOwner,
    pub(crate) workflow_admission: Arc<tokio::sync::Semaphore>,
    pub(crate) group_controls: AsyncMutex<()>,
    pub(crate) url_elicitations: Arc<UrlElicitations>,
    pub(crate) running_requests: Arc<Mutex<HashMap<RequestId, ProcessId>>>,
    pub(crate) session_processes: Arc<AsyncMutex<HashMap<String, ProcessId>>>,
    pub(crate) process_names: Arc<AsyncMutex<HashMap<ProcessId, String>>>,
    pub(crate) conversation: AsyncMutex<()>,
    closed: watch::Sender<bool>,
}

impl McpSessionState {
    fn new(session: &Session, config: &Config, auth: &Arc<AuthManager>) -> Self {
        let table = || {
            Arc::new(ProcessTable::new(
                config,
                auth.clone(),
                SessionSource::Mcp,
                CollaborationModesConfig {
                    default_mode_request_user_input: true,
                },
            ))
        };
        Self {
            sdk_session: session.clone(),
            process_table: table(),
            workflow_process_table: table(),
            workflow_owner: WorkflowOwner::new(session),
            workflow_admission: Arc::new(tokio::sync::Semaphore::new(4)),
            group_controls: AsyncMutex::new(()),
            url_elicitations: Arc::default(),
            running_requests: Arc::default(),
            session_processes: Arc::default(),
            process_names: Arc::default(),
            conversation: AsyncMutex::new(()),
            closed: watch::channel(false).0,
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    fn belongs_to(&self, session: &Session) -> bool {
        // SDK Session clones share immutable handshake metadata. Retaining its
        // handle lets us distinguish a new initialize reusing an old textual
        // ID before the next expiry poll, without trusting caller metadata.
        same_session(&self.sdk_session, session)
    }

    pub(crate) fn close(&self) {
        self.closed.send_replace(true);
        self.workflow_admission.close();
        self.workflow_owner.close();
    }

    pub(crate) fn close_signal(&self) -> watch::Receiver<bool> {
        self.closed.subscribe()
    }

    /// Subscribe before checking the value so close cannot be missed.
    pub(crate) async fn cancelled(&self) {
        let mut closed = self.closed.subscribe();
        if !*closed.borrow_and_update() {
            let _ = closed.changed().await;
        }
    }

    pub(crate) async fn owns_process(&self, id: ProcessId) -> bool {
        !self.is_closed() && self.process_table.get_process(id).await.is_ok()
    }

    async fn shutdown(&self) -> std::io::Result<()> {
        self.close();
        // Let the biased close-select drop a conversation's in-flight startup
        // before taking ProcessTable's shutdown snapshot. A slow config load
        // cannot start a process afterward (the close branch wins first).
        let _conversation =
            tokio::time::timeout(Duration::from_millis(100), self.conversation.lock())
                .await
                .ok();
        // Both tables contain only this owner's processes (including children).
        let (conversation, workflows) = tokio::join!(
            self.process_table
                .shutdown_all_processes_bounded(CLEANUP_TIMEOUT),
            self.workflow_process_table
                .shutdown_all_processes_bounded(CLEANUP_TIMEOUT),
        );
        self.running_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.session_processes.lock().await.clear();
        self.process_names.lock().await.clear();
        if !conversation.submit_failed.is_empty()
            || !conversation.timed_out.is_empty()
            || !workflows.submit_failed.is_empty()
            || !workflows.timed_out.is_empty()
        {
            tracing::warn!(
                ?conversation,
                ?workflows,
                "MCP session process cleanup incomplete"
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "MCP session process cleanup incomplete",
            ));
        }
        Ok(())
    }
}

impl Drop for McpSessionState {
    fn drop(&mut self) {
        // Synchronous safety net also works while the Tokio runtime is exiting.
        self.close();
    }
}

#[derive(Default)]
struct Entries {
    closed: bool,
    sessions: HashMap<String, Arc<McpSessionState>>,
    retired: Vec<Arc<McpSessionState>>,
}

/// Synchronous admission/closure makes shutdown race-free with lazy allocation.
#[derive(Default)]
pub(crate) struct SessionStates(Mutex<Entries>);

impl SessionStates {
    pub(crate) fn get(
        &self,
        session: &Session,
        config: &Config,
        auth: &Arc<AuthManager>,
    ) -> Result<Arc<McpSessionState>, ToolError> {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.closed || !session.is_initialized() {
            return Err(ToolError::Execution(
                "MCP session is closed or uninitialized".into(),
            ));
        }
        if entries
            .sessions
            .get(session.id())
            .is_some_and(|state| !state.belongs_to(session))
            && let Some(old) = entries.sessions.remove(session.id())
        {
            old.close();
            entries.retired.push(old);
        }
        let state = entries
            .sessions
            .entry(session.id().into())
            .or_insert_with(|| Arc::new(McpSessionState::new(session, config, auth)));
        if state.is_closed() {
            return Err(ToolError::Execution("MCP session is closed".into()));
        }
        Ok(state.clone())
    }

    pub(crate) fn close(&self) {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.closed = true;
        for state in entries.sessions.values() {
            state.close();
        }
        for state in &entries.retired {
            state.close();
        }
    }

    /// Only the authoritative SDK session list can expire ownership. Losing an
    /// SSE subscriber is deliberately irrelevant.
    pub(crate) fn expire(&self, server: &Server) -> Vec<Arc<McpSessionState>> {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Snapshot while holding the admission lock. A newly initialized
        // session cannot allocate state between snapshot and pruning.
        let active: HashMap<_, _> = server
            .active_sessions()
            .into_iter()
            .filter(Session::is_initialized)
            .map(|session| (session.id().to_owned(), session))
            .collect();
        let mut expired = std::mem::take(&mut entries.retired);
        entries.sessions.retain(|id, state| {
            if active
                .get(id)
                .is_some_and(|session| state.belongs_to(session))
                && !state.is_closed()
            {
                true
            } else {
                state.close();
                expired.push(state.clone());
                false
            }
        });
        expired
    }

    pub(crate) fn drain(&self) -> Vec<Arc<McpSessionState>> {
        self.close();
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut states = std::mem::take(&mut entries.retired);
        states.extend(entries.sessions.drain().map(|(_, state)| state));
        states
    }
}

pub(crate) fn same_session(left: &Session, right: &Session) -> bool {
    left.id() == right.id()
        && match (left.client_info(), right.client_info()) {
            (Some(left), Some(right)) => std::ptr::eq(left, right),
            _ => false,
        }
}

/// Run cleanup concurrently so a stalled session cannot serialize shutdown of
/// all the others. The outer deadline also bounds lock acquisition.
pub(crate) fn cleanup_sessions(
    cleanup: &mut tokio::task::JoinSet<std::io::Result<()>>,
    states: Vec<Arc<McpSessionState>>,
) {
    for state in states {
        cleanup.spawn(async move {
            tokio::time::timeout(
                CLEANUP_TIMEOUT + Duration::from_millis(250),
                state.shutdown(),
            )
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "MCP session cleanup deadline expired",
                )
            })?
        });
    }
}

#[cfg(test)]
mod tests;
