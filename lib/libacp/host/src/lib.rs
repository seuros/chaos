#![doc = include_str!("../README.md")]

use std::sync::Arc;
use std::time::Duration;

use protocol::schema::v1::{NewSessionRequest, PromptCapabilities, PromptRequest, PromptResponse};

mod framing;
mod server;
mod state;

/// Official SDK types and protocol implementation.
pub use agent_client_protocol as protocol;
pub use server::{AgentServer, AgentServerBuilder};
pub use state::{ClientHandle, SessionContext};

/// Backend-owned creation of isolated native sessions.
pub trait AgentBackend: Send + Sync + 'static {
    /// Additional prompt content supported by this backend.
    fn prompt_capabilities(&self) -> PromptCapabilities {
        PromptCapabilities::new()
    }

    /// Validate session configuration before starting native work.
    ///
    /// In particular, MCP command/environment declarations are untrusted input.
    /// Creation must be cancellation-safe: dropping this future must contain any
    /// partially started native work, even before a session handle is returned.
    fn new_session(
        &self,
        request: NewSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>>;
}

/// One native conversation. The host serializes its prompts.
pub trait AgentSession: Send + Sync + 'static {
    /// Return only after native execution has reached the reported terminal state.
    fn prompt(
        &self,
        request: PromptRequest,
        context: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<PromptResponse>>;

    /// Request interruption. Success is delivery, not proof of termination.
    fn cancel(&self) -> protocol::BoxFuture<'_, protocol::Result<()>>;

    /// Stop and join session-owned work, including descendants and tool execution.
    fn shutdown(&self) -> protocol::BoxFuture<'_, protocol::Result<()>>;
}

/// Resource limits for one connection.
#[derive(Clone, Debug)]
pub struct ConnectionLimits {
    /// Maximum encoded JSON frame size.
    pub max_message_bytes: usize,
    /// Maximum open or opening sessions.
    pub max_sessions: usize,
    /// Maximum queued updates per session, plus the active writer frame.
    pub update_capacity: usize,
    /// Maximum outstanding client permission requests and inbound requests.
    pub pending_requests: usize,
    /// Permission response deadline. Expiration denies and closes the connection.
    pub approval_timeout: Duration,
    /// Maximum time to join native cancellation/shutdown.
    pub shutdown_timeout: Duration,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 1024 * 1024,
            max_sessions: 16,
            update_capacity: 256,
            pending_requests: 32,
            approval_timeout: Duration::from_secs(60),
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

impl ConnectionLimits {
    fn valid(&self) -> bool {
        self.max_message_bytes > 0
            && self.max_sessions > 0
            && self.update_capacity > 0
            && self.pending_requests > 0
            && !self.approval_timeout.is_zero()
            && !self.shutdown_timeout.is_zero()
            && self.max_sessions <= tokio::sync::Semaphore::MAX_PERMITS
            && self.pending_requests <= tokio::sync::Semaphore::MAX_PERMITS
    }
}

/// Transport failure does not establish the outcome of native work.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// Invalid local configuration, before any connection is started.
    #[error("ACP host limits and deadlines must be nonzero and representable")]
    InvalidLimits,
    /// SDK protocol or transport error.
    #[error("ACP protocol error: {0}")]
    Protocol(#[from] protocol::Error),
    /// Bounded connection admission or transport failed.
    #[error("ACP connection closed: {0}")]
    Closed(&'static str),
    /// Native cleanup did not confirm shutdown.
    #[error("ACP native shutdown is unconfirmed")]
    ShutdownUnconfirmed,
}
