#![doc = include_str!("../README.md")]

use std::sync::Arc;
use std::time::Duration;

use protocol::schema::v1::{
    AvailableCommand, ListSessionsRequest, ListSessionsResponse, LoadSessionRequest,
    NewSessionRequest, PromptCapabilities, PromptRequest, PromptResponse, ResumeSessionRequest,
    SessionCapabilities, SessionConfigOption, SessionId, SessionModeState,
    SetSessionConfigOptionRequest,
};

mod capabilities;
mod files;
mod framing;
mod lifecycle;
mod operations;
mod saved;
mod server;
mod state;

/// Official SDK types and protocol implementation.
pub use agent_client_protocol as protocol;
pub use server::{AgentServer, AgentServerBuilder};
pub use state::{ClientHandle, SessionContext};

/// Maximum UTF-8 editor text admitted by the host, before JSON framing.
pub const MAX_EDITOR_TEXT_BYTES: usize = 256 * 1024;

/// Backend-owned identity and the current, caller-filtered session controls.
#[derive(Clone, Default)]
pub struct SessionInfo {
    /// Stable native identity, or `None` to use a connection-local random ID.
    pub id: Option<SessionId>,
    /// Configuration selectors supported by this session.
    pub config_options: Option<Vec<SessionConfigOption>>,
    /// Legacy mode picker, when supported.
    pub modes: Option<SessionModeState>,
    /// Commands that can be invoked through ordinary slash-prefixed prompts.
    pub available_commands: Option<Vec<AvailableCommand>>,
}

fn unsupported() -> protocol::Error {
    protocol::Error::invalid_params().data("ACP backend does not support this operation")
}

/// Backend-owned creation of isolated native sessions.
pub trait AgentBackend: Send + Sync + 'static {
    /// Optional lifecycle capabilities. Defaults advertise none.
    /// Only list, resume and additional-directory support are exposed by this host.
    fn session_capabilities(&self) -> SessionCapabilities {
        SessionCapabilities::new()
    }

    /// Whether this backend can load and publicly replay saved conversations.
    fn load_session_supported(&self) -> bool {
        false
    }

    /// Open a saved session without executing its previous work.
    fn load_session(
        &self,
        _request: LoadSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async { Err(unsupported()) })
    }

    /// Open a saved session without replaying its transcript to the client.
    fn resume_session(
        &self,
        _request: ResumeSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async { Err(unsupported()) })
    }

    /// List only sessions this backend is authorized to expose.
    fn list_sessions(
        &self,
        _request: ListSessionsRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<ListSessionsResponse>> {
        Box::pin(async { Err(unsupported()) })
    }

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
    /// Read current controls and optional durable identity without side effects.
    fn info(&self) -> protocol::BoxFuture<'_, protocol::Result<SessionInfo>> {
        Box::pin(async { Ok(SessionInfo::default()) })
    }

    /// Apply a change through native policy, then return confirmed state.
    ///
    /// Changes are serialized with each other, but may run concurrently with a
    /// prompt. Backends must synchronize their own configuration and native work.
    fn set_config_option(
        &self,
        _request: SetSessionConfigOptionRequest,
        _context: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<SessionInfo>> {
        Box::pin(async { Err(unsupported()) })
    }

    /// Replay only public history, without executing tools or provider calls.
    fn replay(&self, _context: SessionContext) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async { Err(unsupported()) })
    }

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
    /// Permission response deadline. Expiration denies that request.
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
