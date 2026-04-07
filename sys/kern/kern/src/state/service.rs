use std::sync::Arc;

use crate::AuthManager;
use crate::RolloutRecorder;
use crate::catalog::CatalogSink;
use crate::client::ModelClient;
use crate::config::StartedNetworkProxy;
use crate::exec_policy::ExecPolicyManager;
use crate::file_watcher::FileWatcher;
use crate::mcp::McpManager;
use crate::minions::AgentControl;
use crate::models_manager::manager::ModelsManager;
use crate::skills::SkillsManager;
use crate::state_db::StateDbHandle;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::sandboxing::ApprovalStore;
use crate::unified_exec::UnifiedExecProcessManager;
use chaos_dtrace::Hooks;
use chaos_mcp_runtime::manager::McpConnectionManager;
use chaos_syslog::SessionTelemetry;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub(crate) struct SessionServices {
    pub(crate) catalog: Arc<CatalogSink>,
    pub(crate) mcp_connection_manager: Arc<RwLock<McpConnectionManager>>,
    pub(crate) mcp_startup_cancellation_token: Mutex<CancellationToken>,
    pub(crate) unified_exec_manager: UnifiedExecProcessManager,
    pub(crate) hooks: Hooks,
    pub(crate) rollout: Mutex<Option<RolloutRecorder>>,
    pub(crate) user_shell: Arc<crate::shell::Shell>,
    pub(crate) shell_snapshot_tx: watch::Sender<Option<Arc<crate::shell_snapshot::ShellSnapshot>>>,

    pub(crate) exec_policy: ExecPolicyManager,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) models_manager: Arc<ModelsManager>,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) tool_approvals: Mutex<ApprovalStore>,
    pub(crate) skills_manager: Arc<SkillsManager>,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) file_watcher: Arc<FileWatcher>,
    pub(crate) agent_control: AgentControl,
    pub(crate) network_proxy: Option<StartedNetworkProxy>,
    pub(crate) network_approval: Arc<NetworkApprovalService>,
    pub(crate) state_db: Option<StateDbHandle>,
    /// Session-scoped model client shared across turns.
    pub(crate) model_client: ModelClient,
    /// Hallucinate scripting engine handle (Lua/WASM user scripts).
    pub(crate) hallucinate: Option<chaos_hallucinate::HallucinateHandle>,
}
