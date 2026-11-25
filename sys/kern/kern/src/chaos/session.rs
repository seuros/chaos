mod accessors;
mod completions;
mod context;
mod controls;
mod event;
mod history;
mod init;
mod machine_recovery;
mod mcp_notifications;
mod modes;
mod planning;
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
    #[cfg(feature = "toolchain-access")]
    pub(crate) toolchain_access: toolchain_access::Snapshot,
    #[cfg(feature = "toolchain-access")]
    pub(super) approved_toolchain_outputs: std::sync::OnceLock<Vec<std::path::PathBuf>>,
    pub(crate) conversation_id: ProcessId,
    pub(crate) tx_event: Sender<Event>,
    pub(crate) mcp_notification_tx: Sender<McpServerNotification>,
    pub(super) agent_status: watch::Sender<AgentStatus>,
    /// Orders committed child statuses and their live parent publication.
    pub(super) agent_status_publication: Mutex<()>,
    pub(super) out_of_band_elicitation_paused: watch::Sender<bool>,
    pub(crate) state: Mutex<SessionState>,
    pub(crate) active_turn: Mutex<Option<ActiveTurn>>,
    pub(crate) completions: completions::CompletionMailbox,
    pub(crate) permission_actor: PermissionActor,

    pub(crate) services: SessionServices,
    pub(super) next_internal_sub_id: AtomicU64,
    /// Host-owned notice delivered at the next boundary (see `host_notice`).
    pub(crate) host_notice: crate::host_notice::HostNoticeSlot,
}

impl Session {
    pub(crate) fn toolchain_vfs(
        &self,
        policy: chaos_ipc::permissions::VfsPolicy,
        command_cwd: &std::path::Path,
        policy_cwd: &std::path::Path,
    ) -> chaos_ipc::permissions::VfsPolicy {
        #[cfg(not(feature = "toolchain-access"))]
        let _ = (command_cwd, policy_cwd);
        #[cfg(feature = "toolchain-access")]
        let policy = if let Some(approved) = self.approved_toolchain_outputs.get() {
            let mut policy = policy;
            for candidate in self.toolchain_access.candidates_for(command_cwd) {
                if approved.contains(&candidate.directory) {
                    policy = policy.with_tool_output_write(&candidate.directory, policy_cwd);
                }
            }
            policy
        } else {
            policy
        };
        policy
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Executors may retain cloned registry handles. Closing the owner
        // must cancel work without waiting for those handles to disappear.
        self.services.workflow_runs.shutdown();
        self.services.workflow_control.shutdown();
    }
}
