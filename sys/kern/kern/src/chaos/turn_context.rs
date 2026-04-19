use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use jiff::Zoned;
use serde_json::Value;

use chaos_ipc::config_types::ApprovalsReviewer;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::config_types::Personality;
use chaos_ipc::config_types::ReasoningSummary as ReasoningSummaryConfig;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::TurnContextItem;
use chaos_ipc::protocol::TurnContextNetworkItem;
use chaos_parole::sandbox::vfs_policy_from_sandbox_policy;

use chaos_ready::ReadinessFlag;
use chaos_syslog::SessionTelemetry;
use chaos_syslog::current_span_trace_id;

use crate::AuthManager;
use crate::ModelProviderInfo;
use crate::compact;
use crate::config::Config;
use crate::config::Constrained;
use crate::config::ConstraintResult;
use crate::config::GhostSnapshotConfig;
use crate::config::types::ShellEnvironmentPolicy;
use crate::models_manager::manager::ModelsManager;
use crate::models_manager::manager::RefreshStrategy;
use crate::shell_snapshot::ShellSnapshot;
use crate::tools::spec::ToolsConfig;
use crate::tools::spec::ToolsConfigParams;
use crate::truncate::TruncationPolicy;
use crate::turn_metadata::TurnMetadataState;
use crate::turn_timing::TurnTimingState;
use chaos_pf::NetworkProxy;

/// Notes from the previous real user turn.
///
/// Conceptually this is the same role that `previous_model` used to fill, but
/// it can carry other prior-turn settings that matter when constructing
/// sensible state-change diffs or full-context reinjection, such as model
/// switches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviousTurnSettings {
    pub(crate) model: String,
}

/// The context needed for a single turn of the thread.
#[derive(Debug)]
pub(crate) struct TurnContext {
    pub(crate) sub_id: String,
    pub(crate) trace_id: Option<String>,
    pub(crate) config: Arc<Config>,
    pub(crate) auth_manager: Option<Arc<AuthManager>>,
    pub(crate) model_info: ModelInfo,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) provider: ModelProviderInfo,
    pub(crate) reasoning_effort: Option<ReasoningEffortConfig>,
    pub(crate) reasoning_summary: ReasoningSummaryConfig,
    pub(crate) session_source: SessionSource,
    /// The session's current working directory. All relative paths provided by
    /// the model as well as sandbox policies are resolved against this path
    /// instead of `std::env::current_dir()`.
    pub(crate) cwd: PathBuf,
    pub(crate) current_date: Option<String>,
    pub(crate) timezone: Option<String>,
    pub(crate) app_server_client_name: Option<String>,
    pub(crate) minion_instructions: Option<String>,
    pub(crate) compact_prompt: Option<String>,
    pub(crate) user_instructions: Option<String>,
    pub(crate) collaboration_mode: CollaborationMode,
    pub(crate) personality: Option<Personality>,
    pub(crate) approval_policy: Constrained<ApprovalPolicy>,
    pub(crate) vfs_policy: VfsPolicy,
    pub(crate) socket_policy: SocketPolicy,
    pub(crate) network: Option<NetworkProxy>,
    pub(crate) shell_environment_policy: ShellEnvironmentPolicy,
    pub(crate) tools_config: ToolsConfig,
    pub(crate) features: crate::config::ManagedFeatures,
    pub(crate) ghost_snapshot: GhostSnapshotConfig,
    pub(crate) final_output_json_schema: Option<Value>,
    pub(crate) alcatraz_macos_exe: Option<PathBuf>,
    pub(crate) alcatraz_linux_exe: Option<PathBuf>,
    pub(crate) alcatraz_freebsd_exe: Option<PathBuf>,
    pub(crate) tool_call_gate: Arc<ReadinessFlag>,
    pub(crate) truncation_policy: TruncationPolicy,
    pub(crate) dynamic_tools: Vec<chaos_ipc::dynamic_tools::DynamicToolSpec>,
    pub(crate) turn_metadata_state: Arc<TurnMetadataState>,
    pub(crate) turn_timing_state: Arc<TurnTimingState>,
}

impl TurnContext {
    pub(crate) fn model_context_window(&self) -> Option<i64> {
        let effective_context_window_percent = self.model_info.effective_context_window_percent;
        self.model_info.context_window.map(|context_window| {
            context_window.saturating_mul(effective_context_window_percent) / 100
        })
    }

    pub(crate) fn apps_enabled(&self) -> bool {
        false
    }

    pub(crate) async fn with_model(&self, model: String, models_manager: &ModelsManager) -> Self {
        let mut config = (*self.config).clone();
        config.model = Some(model.clone());
        let model_info = models_manager.get_model_info(model.as_str(), &config).await;
        let truncation_policy = model_info.truncation_policy.into();
        let supported_reasoning_levels = model_info
            .supported_reasoning_levels
            .iter()
            .map(|preset| preset.effort)
            .collect::<Vec<_>>();
        let reasoning_effort = if let Some(current_reasoning_effort) = self.reasoning_effort {
            if supported_reasoning_levels.contains(&current_reasoning_effort) {
                Some(current_reasoning_effort)
            } else {
                supported_reasoning_levels
                    .get(supported_reasoning_levels.len().saturating_sub(1) / 2)
                    .copied()
                    .or(model_info.default_reasoning_level)
            }
        } else {
            supported_reasoning_levels
                .get(supported_reasoning_levels.len().saturating_sub(1) / 2)
                .copied()
                .or(model_info.default_reasoning_level)
        };
        config.model_reasoning_effort = reasoning_effort;

        let collaboration_mode = self.collaboration_mode.with_updates(
            Some(model.clone()),
            Some(reasoning_effort),
            /*minion_instructions*/ None,
        );
        let features = self.features.clone();
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            available_models: &models_manager
                .list_models(RefreshStrategy::OnlineIfUncached)
                .await,
            features: &features,
            web_search_mode: self.tools_config.web_search_mode,
            session_source: self.session_source.clone(),
            vfs_policy: &self.vfs_policy,
            collab_enabled: config.collab_enabled,
        })
        .with_unified_exec_shell_mode(self.tools_config.unified_exec_shell_mode.clone())
        .with_web_search_config(self.tools_config.web_search_config.clone())
        .with_allow_login_shell(self.tools_config.allow_login_shell)
        .with_agent_roles(config.agent_roles.clone());

        Self {
            sub_id: self.sub_id.clone(),
            trace_id: self.trace_id.clone(),
            config: Arc::new(config),
            auth_manager: self.auth_manager.clone(),
            model_info: model_info.clone(),
            session_telemetry: self
                .session_telemetry
                .clone()
                .with_model(model.as_str(), model_info.slug.as_str()),
            provider: self.provider.clone(),
            reasoning_effort,
            reasoning_summary: self.reasoning_summary,
            session_source: self.session_source.clone(),
            cwd: self.cwd.clone(),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            app_server_client_name: self.app_server_client_name.clone(),
            minion_instructions: self.minion_instructions.clone(),
            compact_prompt: self.compact_prompt.clone(),
            user_instructions: self.user_instructions.clone(),
            collaboration_mode,
            personality: self.personality,
            approval_policy: self.approval_policy.clone(),
            vfs_policy: self.vfs_policy.clone(),
            socket_policy: self.socket_policy,
            network: self.network.clone(),
            shell_environment_policy: self.shell_environment_policy.clone(),
            tools_config,
            features,
            ghost_snapshot: self.ghost_snapshot.clone(),
            final_output_json_schema: self.final_output_json_schema.clone(),
            alcatraz_macos_exe: self.alcatraz_macos_exe.clone(),
            alcatraz_linux_exe: self.alcatraz_linux_exe.clone(),
            alcatraz_freebsd_exe: self.alcatraz_freebsd_exe.clone(),
            tool_call_gate: Arc::new(ReadinessFlag::new()),
            truncation_policy,
            dynamic_tools: self.dynamic_tools.clone(),
            turn_metadata_state: self.turn_metadata_state.clone(),
            turn_timing_state: Arc::clone(&self.turn_timing_state),
        }
    }

    pub(crate) fn resolve_path(&self, path: Option<String>) -> PathBuf {
        path.as_ref()
            .map(PathBuf::from)
            .map_or_else(|| self.cwd.clone(), |p| self.cwd.join(p))
    }

    pub(crate) fn compact_prompt(&self) -> &str {
        self.compact_prompt
            .as_deref()
            .unwrap_or(compact::SUMMARIZATION_PROMPT)
    }

    pub(crate) fn to_turn_context_item(&self) -> TurnContextItem {
        TurnContextItem {
            turn_id: Some(self.sub_id.clone()),
            trace_id: self.trace_id.clone(),
            cwd: self.cwd.clone(),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            approval_policy: self.approval_policy.value(),
            vfs_policy: self.vfs_policy.clone(),
            socket_policy: self.socket_policy,
            network: self.turn_context_network_item(),
            model: self.model_info.slug.clone(),
            personality: self.personality,
            collaboration_mode: Some(self.collaboration_mode.clone()),
            effort: self.reasoning_effort,
            summary: self.reasoning_summary,
            user_instructions: self.user_instructions.clone(),
            minion_instructions: self.minion_instructions.clone(),
            final_output_json_schema: self.final_output_json_schema.clone(),
            truncation_policy: Some(self.truncation_policy.into()),
        }
    }

    fn turn_context_network_item(&self) -> Option<TurnContextNetworkItem> {
        let network = self
            .config
            .config_layer_stack
            .requirements()
            .network
            .as_ref()?;
        Some(TurnContextNetworkItem {
            allowed_domains: network.allowed_domains.clone().unwrap_or_default(),
            denied_domains: network.denied_domains.clone().unwrap_or_default(),
        })
    }
}

pub(super) fn local_time_context() -> (String, String) {
    match iana_time_zone::get_timezone() {
        Ok(timezone) => (Zoned::now().strftime("%Y-%m-%d").to_string(), timezone),
        Err(_) => (
            Timestamp::now()
                .to_zoned(jiff::tz::TimeZone::UTC)
                .strftime("%Y-%m-%d")
                .to_string(),
            "Etc/UTC".to_string(),
        ),
    }
}

#[derive(Clone)]
pub(crate) struct SessionConfiguration {
    /// Provider identifier ("openai", "openrouter", ...).
    pub(super) provider: ModelProviderInfo,

    pub(super) collaboration_mode: CollaborationMode,
    pub(super) model_reasoning_summary: Option<ReasoningSummaryConfig>,
    pub(super) service_tier: Option<ServiceTier>,

    /// Minion instructions that supplement the base instructions.
    pub(super) minion_instructions: Option<String>,

    /// Model instructions that are appended to the base instructions.
    pub(super) user_instructions: Option<String>,

    /// Personality preference for the model.
    pub(super) personality: Option<Personality>,

    /// Base instructions for the session.
    pub(super) base_instructions: String,

    /// Compact prompt override.
    pub(super) compact_prompt: Option<String>,

    /// When to escalate for approval for execution
    pub(super) approval_policy: Constrained<ApprovalPolicy>,
    pub(super) approvals_reviewer: ApprovalsReviewer,
    pub(super) vfs_policy: VfsPolicy,
    pub(super) socket_policy: SocketPolicy,

    /// Working directory that should be treated as the *root* of the
    /// session. All relative paths supplied by the model as well as the
    /// execution sandbox are resolved against this directory **instead**
    /// of the process-wide current working directory. CLI front-ends are
    /// expected to expand this to an absolute path before sending the
    /// `ConfigureSession` operation so that the business-logic layer can
    /// operate deterministically.
    pub(super) cwd: PathBuf,
    /// Directory containing all Chaos state for this session.
    pub(super) chaos_home: PathBuf,
    /// Optional user-facing name for the thread, updated during the session.
    pub(super) process_name: Option<String>,

    // TODO(pakrym): Remove config from here
    pub(super) original_config_do_not_use: Arc<Config>,
    /// Optional service name tag for session metrics.
    pub(super) metrics_service_name: Option<String>,
    pub(super) app_server_client_name: Option<String>,
    /// Source of the session (cli, vscode, exec, mcp, ...)
    pub(super) session_source: SessionSource,
    pub(super) dynamic_tools: Vec<chaos_ipc::dynamic_tools::DynamicToolSpec>,
    pub(super) persist_extended_history: bool,
    pub(super) inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
}

impl SessionConfiguration {
    pub(crate) fn chaos_home(&self) -> &PathBuf {
        &self.chaos_home
    }

    pub(crate) fn process_config_snapshot(&self) -> crate::process::ProcessConfigSnapshot {
        crate::process::ProcessConfigSnapshot {
            model: self.collaboration_mode.model().to_string(),
            model_provider_id: self.original_config_do_not_use.model_provider_id.clone(),
            service_tier: self.service_tier,
            approval_policy: self.approval_policy.value(),
            approvals_reviewer: self.approvals_reviewer,
            vfs_policy: self.vfs_policy.clone(),
            socket_policy: self.socket_policy,
            cwd: self.cwd.clone(),
            ephemeral: self.original_config_do_not_use.ephemeral,
            reasoning_effort: self.collaboration_mode.reasoning_effort(),
            personality: self.personality,
            session_source: self.session_source.clone(),
        }
    }

    pub(crate) fn apply(&self, updates: &SessionSettingsUpdate) -> ConstraintResult<Self> {
        let mut next_configuration = self.clone();
        if let Some(collaboration_mode) = updates.collaboration_mode.clone() {
            next_configuration.collaboration_mode = collaboration_mode;
        }
        if let Some(summary) = updates.reasoning_summary {
            next_configuration.model_reasoning_summary = Some(summary);
        }
        if let Some(service_tier) = updates.service_tier {
            next_configuration.service_tier = service_tier;
        }
        if let Some(personality) = updates.personality {
            next_configuration.personality = Some(personality);
        }
        if let Some(approval_policy) = updates.approval_policy {
            next_configuration.approval_policy.set(approval_policy)?;
        }
        if let Some(approvals_reviewer) = updates.approvals_reviewer {
            next_configuration.approvals_reviewer = approvals_reviewer;
        }
        if let Some(sandbox_policy) = updates.sandbox_policy.clone() {
            next_configuration.vfs_policy =
                vfs_policy_from_sandbox_policy(&sandbox_policy, &self.cwd);
            next_configuration.socket_policy = SocketPolicy::from(&sandbox_policy);
        }
        if let Some(cwd) = updates.cwd.clone() {
            next_configuration.cwd = cwd;
        }
        if let Some(app_server_client_name) = updates.app_server_client_name.clone() {
            next_configuration.app_server_client_name = Some(app_server_client_name);
        }
        Ok(next_configuration)
    }
}

#[derive(Default, Clone)]
pub(crate) struct SessionSettingsUpdate {
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) approval_policy: Option<ApprovalPolicy>,
    pub(crate) approvals_reviewer: Option<ApprovalsReviewer>,
    pub(crate) sandbox_policy: Option<SandboxPolicy>,
    pub(crate) collaboration_mode: Option<CollaborationMode>,
    pub(crate) reasoning_summary: Option<ReasoningSummaryConfig>,
    pub(crate) service_tier: Option<Option<ServiceTier>>,
    pub(crate) final_output_json_schema: Option<Option<Value>>,
    pub(crate) personality: Option<Personality>,
    pub(crate) app_server_client_name: Option<String>,
}

/// Construct a `TurnContext` from a `SessionConfiguration`.
///
/// This is the static constructor used by `Session::make_turn_context`.
#[allow(clippy::too_many_arguments)]
pub(super) fn make_turn_context(
    auth_manager: Option<Arc<AuthManager>>,
    session_telemetry: &SessionTelemetry,
    provider: ModelProviderInfo,
    session_configuration: &SessionConfiguration,
    per_turn_config: Config,
    model_info: ModelInfo,
    models_manager: &ModelsManager,
    network: Option<NetworkProxy>,
    sub_id: String,
) -> TurnContext {
    let reasoning_effort = session_configuration.collaboration_mode.reasoning_effort();
    let reasoning_summary = session_configuration
        .model_reasoning_summary
        .unwrap_or(model_info.default_reasoning_summary);
    let session_telemetry = session_telemetry.clone().with_model(
        session_configuration.collaboration_mode.model(),
        model_info.slug.as_str(),
    );
    let session_source = session_configuration.session_source.clone();
    let auth_manager_for_context = auth_manager;
    let provider_for_context = provider;
    let session_telemetry_for_context = session_telemetry;
    let per_turn_config = Arc::new(per_turn_config);

    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &models_manager.try_list_models().unwrap_or_default(),
        features: &per_turn_config.features,
        web_search_mode: Some(per_turn_config.web_search_mode.value()),
        session_source: session_source.clone(),
        vfs_policy: &session_configuration.vfs_policy,
        collab_enabled: per_turn_config.collab_enabled,
    })
    .with_web_search_config(per_turn_config.web_search_config.clone())
    .with_allow_login_shell(per_turn_config.permissions.allow_login_shell)
    .with_agent_roles(per_turn_config.agent_roles.clone());

    let cwd = session_configuration.cwd.clone();
    let turn_metadata_state = Arc::new(TurnMetadataState::new(
        sub_id.clone(),
        cwd.clone(),
        &session_configuration.vfs_policy,
    ));
    let (current_date, timezone) = local_time_context();
    TurnContext {
        sub_id,
        trace_id: current_span_trace_id(),
        config: per_turn_config.clone(),
        auth_manager: auth_manager_for_context,
        model_info: model_info.clone(),
        session_telemetry: session_telemetry_for_context,
        provider: provider_for_context,
        reasoning_effort,
        reasoning_summary,
        session_source,
        cwd,
        current_date: Some(current_date),
        timezone: Some(timezone),
        app_server_client_name: session_configuration.app_server_client_name.clone(),
        minion_instructions: session_configuration.minion_instructions.clone(),
        compact_prompt: session_configuration.compact_prompt.clone(),
        user_instructions: session_configuration.user_instructions.clone(),
        collaboration_mode: session_configuration.collaboration_mode.clone(),
        personality: session_configuration.personality,
        approval_policy: session_configuration.approval_policy.clone(),
        vfs_policy: session_configuration.vfs_policy.clone(),
        socket_policy: session_configuration.socket_policy,
        network,
        shell_environment_policy: per_turn_config.permissions.shell_environment_policy.clone(),
        tools_config,
        features: per_turn_config.features.clone(),
        ghost_snapshot: per_turn_config.ghost_snapshot.clone(),
        final_output_json_schema: None,
        alcatraz_macos_exe: per_turn_config.alcatraz_macos_exe.clone(),
        alcatraz_linux_exe: per_turn_config.alcatraz_linux_exe.clone(),
        alcatraz_freebsd_exe: per_turn_config.alcatraz_freebsd_exe.clone(),
        tool_call_gate: Arc::new(ReadinessFlag::new()),
        truncation_policy: model_info.truncation_policy.into(),
        dynamic_tools: session_configuration.dynamic_tools.clone(),
        turn_metadata_state,
        turn_timing_state: Arc::new(TurnTimingState::default()),
    }
}
