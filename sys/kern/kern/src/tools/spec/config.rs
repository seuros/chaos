use std::collections::BTreeMap;

use chaos_ipc::config_types::WebSearchConfig;
use chaos_ipc::config_types::WebSearchMode;
use chaos_ipc::openai_models::ApplyPatchToolType;
use chaos_ipc::openai_models::ConfigShellToolType;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ModelPreset;
use chaos_ipc::openai_models::WebSearchToolType;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;

use crate::config::AgentRoleConfig;
use crate::features::Feature;
use crate::features::Features;
use crate::original_image_detail::can_request_original_image_detail;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum UnifiedExecShellMode {
    Direct,
}

#[derive(Debug, Clone)]
pub(crate) struct ToolsConfig {
    pub available_models: Vec<ModelPreset>,
    pub shell_type: ConfigShellToolType,
    pub unified_exec_shell_mode: UnifiedExecShellMode,
    pub allow_login_shell: bool,
    pub apply_patch_tool_type: Option<ApplyPatchToolType>,
    pub web_search_mode: Option<WebSearchMode>,
    pub web_search_config: Option<WebSearchConfig>,
    pub web_search_tool_type: WebSearchToolType,
    pub image_gen_tool: bool,
    pub agent_roles: BTreeMap<String, AgentRoleConfig>,
    pub exec_permission_approvals_enabled: bool,
    pub request_permissions_tool_enabled: bool,
    pub can_request_original_image_detail: bool,
    pub collab_tools: bool,
    pub request_user_input: bool,
    pub default_mode_request_user_input: bool,
    pub experimental_supported_tools: Vec<String>,
    pub agent_jobs_tools: bool,
    pub agent_jobs_worker_tools: bool,
    /// Native server-side tools declared by the model/provider ABI.
    pub native_server_side_tools: Vec<String>,
}

pub(crate) struct ToolsConfigParams<'a> {
    pub(crate) model_info: &'a ModelInfo,
    pub(crate) available_models: &'a Vec<ModelPreset>,
    pub(crate) features: &'a Features,
    pub(crate) web_search_mode: Option<WebSearchMode>,
    pub(crate) session_source: SessionSource,
    pub(crate) sandbox_policy: &'a SandboxPolicy,
    pub(crate) collab_enabled: bool,
}

fn unified_exec_allowed_in_environment(_sandbox_policy: &SandboxPolicy) -> bool {
    true
}

impl ToolsConfig {
    pub fn new(params: &ToolsConfigParams) -> Self {
        let ToolsConfigParams {
            model_info,
            available_models: available_models_ref,
            features,
            web_search_mode,
            session_source,
            sandbox_policy,
            collab_enabled,
        } = params;
        let include_collab_tools = *collab_enabled;
        let include_agent_jobs = features.enabled(Feature::SpawnCsv);
        let include_request_user_input = !matches!(session_source, SessionSource::SubAgent(_));
        let include_default_mode_request_user_input = include_request_user_input;
        let include_original_image_detail = can_request_original_image_detail(model_info);
        let include_image_gen_tool = false;
        let exec_permission_approvals_enabled = features.enabled(Feature::ExecPermissionApprovals);
        let request_permissions_tool_enabled = features.enabled(Feature::RequestPermissionsTool);
        let unified_exec_allowed = unified_exec_allowed_in_environment(sandbox_policy);
        let shell_type = if unified_exec_allowed {
            ConfigShellToolType::UnifiedExec
        } else if model_info.shell_type == ConfigShellToolType::UnifiedExec {
            ConfigShellToolType::ShellCommand
        } else {
            model_info.shell_type
        };

        let apply_patch_tool_type = model_info.apply_patch_tool_type.clone();

        let agent_jobs_worker_tools = include_agent_jobs
            && matches!(
                session_source,
                SessionSource::SubAgent(SubAgentSource::Other(label))
                    if label.starts_with("agent_job:")
            );

        Self {
            available_models: available_models_ref.to_vec(),
            shell_type,
            unified_exec_shell_mode: UnifiedExecShellMode::Direct,
            allow_login_shell: true,
            apply_patch_tool_type,
            web_search_mode: *web_search_mode,
            web_search_config: None,
            web_search_tool_type: model_info.web_search_tool_type,
            image_gen_tool: include_image_gen_tool,
            agent_roles: BTreeMap::new(),
            exec_permission_approvals_enabled,
            request_permissions_tool_enabled,
            can_request_original_image_detail: include_original_image_detail,
            collab_tools: include_collab_tools,
            request_user_input: include_request_user_input,
            default_mode_request_user_input: include_default_mode_request_user_input,
            experimental_supported_tools: model_info.experimental_supported_tools.clone(),
            agent_jobs_tools: include_agent_jobs,
            agent_jobs_worker_tools,
            native_server_side_tools: model_info.native_server_side_tools.clone(),
        }
    }

    pub fn with_agent_roles(mut self, agent_roles: BTreeMap<String, AgentRoleConfig>) -> Self {
        self.agent_roles = agent_roles;
        self
    }

    pub fn with_allow_login_shell(mut self, allow_login_shell: bool) -> Self {
        self.allow_login_shell = allow_login_shell;
        self
    }

    pub fn with_unified_exec_shell_mode(
        mut self,
        unified_exec_shell_mode: UnifiedExecShellMode,
    ) -> Self {
        self.unified_exec_shell_mode = unified_exec_shell_mode;
        self
    }

    pub fn with_web_search_config(mut self, web_search_config: Option<WebSearchConfig>) -> Self {
        self.web_search_config = web_search_config;
        self
    }
}
