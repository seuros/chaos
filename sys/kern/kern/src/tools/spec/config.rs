use std::collections::BTreeMap;

use chaos_ipc::config_types::WebSearchConfig;
use chaos_ipc::config_types::WebSearchMode;
use chaos_ipc::openai_models::ApplyPatchToolType;
use chaos_ipc::openai_models::ConfigShellToolType;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ModelPreset;
use chaos_ipc::openai_models::WebSearchToolType;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;

use crate::config::AgentRoleConfig;
use crate::modes::ModeCapabilities;
use crate::original_image_detail::can_request_original_image_detail;

#[derive(Debug, Clone)]
pub(crate) struct ToolsConfig {
    pub model_tools_disabled: bool,
    pub machine_recovery: bool,
    pub recall_available: bool,
    pub available_models: Vec<ModelPreset>,
    pub shell_type: ConfigShellToolType,
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
    pub supervisor_tool: bool,
    pub request_user_input: bool,
    pub default_mode_request_user_input: bool,
    pub dynamic_parent_effort: bool,
    pub experimental_supported_tools: Vec<String>,
    pub minion_jobs_tools: bool,
    pub minion_jobs_worker_tools: bool,
    pub agent_compaction_control: bool,
    pub agent_session_title: bool,
    pub mode_switching: bool,
    pub planning_records: bool,
    pub planning_authoring: bool,
    pub planning: chaos_proc::planning::PlanningCapabilities,
    pub attached_plan: Option<String>,
    pub mode_allow_dynamic_tools: bool,
    /// Native server-side tools declared by the model/provider ABI.
    pub native_server_side_tools: Vec<String>,
}

pub(crate) struct ToolsConfigParams<'a> {
    pub(crate) model_info: &'a ModelInfo,
    pub(crate) available_models: &'a Vec<ModelPreset>,
    pub(crate) approval_policy: ApprovalPolicy,
    pub(crate) minion_jobs_allowed: bool,
    pub(crate) web_search_mode: Option<WebSearchMode>,
    pub(crate) session_source: SessionSource,
    pub(crate) collab_enabled: bool,
}

impl ToolsConfig {
    pub fn new(params: &ToolsConfigParams) -> Self {
        let ToolsConfigParams {
            model_info,
            available_models: available_models_ref,
            approval_policy,
            minion_jobs_allowed,
            web_search_mode,
            session_source,
            collab_enabled,
        } = params;
        let include_collab_tools = *collab_enabled;
        let include_supervisor_tool =
            matches!(
                session_source,
                SessionSource::SubAgent(SubAgentSource::ProcessSpawn { .. })
            ) && !crate::minions::is_internal_process_spawn(session_source);
        let include_minion_jobs = *minion_jobs_allowed;
        let include_request_user_input = !matches!(session_source, SessionSource::SubAgent(_));
        let include_default_mode_request_user_input = include_request_user_input;
        let include_original_image_detail = can_request_original_image_detail(model_info);
        let include_image_gen_tool = false;
        let model_tools_disabled = matches!(
            session_source,
            SessionSource::SubAgent(SubAgentSource::Other(label))
                if label == "session_title_review"
        );
        let exec_permission_approvals_enabled = approval_policy.allows_escalation();
        let request_permissions_tool_enabled =
            approval_policy.advertises_request_permissions_tool();
        // Managed exec is available on every supported platform. Sandbox policy
        // is enforced at invocation time, not by selecting another backend.
        let shell_type = ConfigShellToolType::Exec;

        let apply_patch_tool_type = model_info.apply_patch_tool_type.clone();

        let minion_jobs_worker_tools = include_minion_jobs
            && matches!(
                session_source,
                SessionSource::SubAgent(SubAgentSource::Other(label))
                    if label.starts_with("minion_job:")
            );

        Self {
            model_tools_disabled,
            machine_recovery: false,
            recall_available: false,
            available_models: available_models_ref.to_vec(),
            shell_type,
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
            supervisor_tool: include_supervisor_tool,
            request_user_input: include_request_user_input,
            default_mode_request_user_input: include_default_mode_request_user_input,
            dynamic_parent_effort: false,
            experimental_supported_tools: model_info.experimental_supported_tools.clone(),
            minion_jobs_tools: include_minion_jobs,
            minion_jobs_worker_tools,
            agent_compaction_control: false,
            agent_session_title: false,
            mode_switching: false,
            planning_records: true,
            planning_authoring: false,
            planning: Default::default(),
            attached_plan: None,
            mode_allow_dynamic_tools: true,
            native_server_side_tools: model_info.native_server_side_tools.clone(),
        }
    }

    pub fn with_agent_roles(mut self, agent_roles: BTreeMap<String, AgentRoleConfig>) -> Self {
        self.agent_roles = agent_roles;
        self
    }

    pub fn with_agent_compaction_control(mut self, enabled: bool) -> Self {
        self.agent_compaction_control = enabled;
        self
    }

    pub fn with_agent_session_title(
        mut self,
        enabled: bool,
        session_source: &SessionSource,
    ) -> Self {
        self.agent_session_title = enabled && !matches!(session_source, SessionSource::SubAgent(_));
        self
    }

    pub fn with_allow_login_shell(mut self, allow_login_shell: bool) -> Self {
        self.allow_login_shell = allow_login_shell;
        self
    }

    pub fn with_web_search_config(mut self, web_search_config: Option<WebSearchConfig>) -> Self {
        self.web_search_config = web_search_config;
        self
    }

    pub fn with_dynamic_parent_effort(
        mut self,
        enabled: bool,
        session_source: &SessionSource,
    ) -> Self {
        self.dynamic_parent_effort =
            enabled && !matches!(session_source, SessionSource::SubAgent(_));
        self
    }

    pub fn with_mode_policy(
        mut self,
        capabilities: ModeCapabilities,
        switching_allowed: bool,
    ) -> Self {
        self.mode_switching = switching_allowed;
        self.planning_records = capabilities.planning_records;
        self.planning_authoring = capabilities.planning_records && !capabilities.mutation;
        self.mode_allow_dynamic_tools = capabilities.mutation;
        self.request_user_input &= capabilities.request_user_input;
        if !capabilities.mutation {
            self.apply_patch_tool_type = None;
            self.request_permissions_tool_enabled = false;
            self.exec_permission_approvals_enabled = false;
            self.agent_session_title = false;
        }
        self
    }
}
