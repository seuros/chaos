mod accessors;
mod context;
mod event;
mod history;
mod init;
mod mcp_notifications;
mod modes;
pub(crate) mod tokens;
mod turn;

use std::sync::atomic::AtomicU64;

use async_channel::Sender;
use tokio::sync::Mutex;
use tokio::sync::watch;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::Event;
use chaos_mcp_runtime::McpServerNotification;

use crate::chaos::permissions::PermissionActor;
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
    pub(crate) mcp_notification_tx: Sender<McpServerNotification>,
    pub(super) agent_status: watch::Sender<AgentStatus>,
    pub(super) out_of_band_elicitation_paused: watch::Sender<bool>,
    pub(crate) state: Mutex<SessionState>,
    pub(crate) active_turn: Mutex<Option<ActiveTurn>>,
    pub(crate) permission_actor: PermissionActor,

    pub(crate) services: SessionServices,
    pub(super) next_internal_sub_id: AtomicU64,
}
