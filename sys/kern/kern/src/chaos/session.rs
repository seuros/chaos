mod accessors;
mod context;
mod event;
mod history;
mod init;
mod tokens;
mod turn;

use std::sync::atomic::AtomicU64;

use async_channel::Sender;
use tokio::sync::Mutex;
use tokio::sync::watch;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::Event;

use crate::config::ManagedFeatures;
use crate::minions::AgentStatus;
use crate::state::ActiveTurn;
use crate::state::SessionServices;
use crate::state::SessionState;

/// Context for an initialized model agent
///
/// A session has at most 1 running task at a time, and can be interrupted by
/// user input.
pub(crate) struct Session {
    pub(crate) conversation_id: ProcessId,
    pub(crate) tx_event: Sender<Event>,
    pub(super) agent_status: watch::Sender<AgentStatus>,
    pub(super) out_of_band_elicitation_paused: watch::Sender<bool>,
    pub(crate) state: Mutex<SessionState>,
    /// The set of enabled features should be invariant for the lifetime of the
    /// session.
    pub(crate) features: ManagedFeatures,
    pub(crate) pending_mcp_server_refresh_config:
        Mutex<Option<chaos_ipc::protocol::McpServerRefreshConfig>>,
    pub(crate) active_turn: Mutex<Option<ActiveTurn>>,

    pub(crate) services: SessionServices,
    pub(super) next_internal_sub_id: AtomicU64,
}
