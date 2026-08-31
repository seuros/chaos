use std::sync::Arc;

use crate::AuthManager;
use crate::RolloutRecorder;
use crate::catalog::CatalogSink;
use crate::client::ModelClient;
use crate::config::StartedNetworkProxy;
use crate::exec_policy::ExecPolicyManager;
use crate::file_watcher::FileWatcher;
use crate::internal_tasks::InternalTaskStore;
use crate::mcp::McpManager;
use crate::mcp_registry::McpRefreshActor;
use crate::mcp_registry::McpRegistryActor;
use crate::minions::AgentControl;
use crate::models_manager::manager::ModelsManager;
use crate::runtime_db::RuntimeDbHandle;
use crate::shell_snapshot::ShellSnapshotActor;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::sandboxing::ApprovalStore;
use crate::unified_exec::UnifiedExecProcessManager;
use chaos_dtrace::Hooks;
use chaos_snitch::SessionTelemetry;
use tokio::sync::Mutex;

pub(crate) struct SessionServices {
    pub(crate) catalog: Arc<CatalogSink>,
    pub(crate) mcp_registry: McpRegistryActor,
    pub(crate) mcp_refresh: McpRefreshActor,
    pub(crate) internal_task_store: InternalTaskStore,
    pub(crate) unified_exec_manager: UnifiedExecProcessManager,
    pub(crate) hooks: Hooks,
    pub(crate) rollout: Mutex<Option<RolloutRecorder>>,
    pub(crate) user_shell: Arc<crate::shell::Shell>,
    pub(crate) shell_snapshot: ShellSnapshotActor,

    pub(crate) exec_policy: ExecPolicyManager,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) models_manager: Arc<ModelsManager>,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) tool_approvals: Mutex<ApprovalStore>,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) file_watcher: Arc<FileWatcher>,
    pub(crate) agent_control: AgentControl,
    pub(crate) network_proxy: Option<StartedNetworkProxy>,
    pub(crate) network_approval: Arc<NetworkApprovalService>,
    pub(crate) runtime_db: Option<RuntimeDbHandle>,
    /// Session-scoped model client shared across turns.
    pub(crate) model_client: ModelClient,
    /// Halluacinate scripting engine handle (Lua/WASM user scripts).
    pub(crate) halluacinate: Option<chaos_halluacinate::HalluacinateHandle>,
}
