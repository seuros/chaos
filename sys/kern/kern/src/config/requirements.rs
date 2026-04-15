use std::path::Path;
use std::path::PathBuf;

use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::config_types::WebSearchMode;
use chaos_ipc::permissions::FileSystemSandboxPolicy;
use chaos_ipc::permissions::NetworkSandboxPolicy;
use chaos_realpath::AbsolutePathBuf;

use crate::config::Config;
use crate::config::ConfigOverrides;
use crate::config::ConfigToml;
use crate::config::ManagedFeatures;
use crate::config::Permissions;
use crate::config::agent_roles;
use crate::config::parsing;
use crate::config::permissions::compile_permission_profile;
use crate::config::permissions::network_proxy_config_from_profile_network;
use crate::config::permissions::resolve_permission_profile;
use crate::config::types::DEFAULT_OTEL_ENVIRONMENT;
use crate::config::types::MemoriesConfig;
use crate::config::types::OtelConfig;
use crate::config::types::OtelConfigToml;
use crate::config::types::OtelExporterKind;
use crate::config::types::UriBasedFileOpener;
use crate::config::validation;
use crate::config::validation::PermissionConfigSyntax;
use crate::config_loader::ConfigLayerStack;
use crate::config_loader::ConfigRequirements;
use crate::config_loader::Sourced;
use crate::features::FeatureOverrides;
use crate::model_provider_info::built_in_model_providers;
use crate::path_utils::normalize_for_native_workdir;
use crate::protocol::ApprovalPolicy;
use crate::protocol::SandboxPolicy;
use chaos_pf::NetworkProxyConfig;

use super::ApprovalsReviewer;
use super::GhostSnapshotConfig;
use super::NetworkProxySpec;
use super::ProjectConfig;
use super::RealtimeAudioConfig;
use super::RealtimeConfig;

use super::{
    DEFAULT_AGENT_JOB_MAX_RUNTIME_SECONDS, DEFAULT_AGENT_MAX_DEPTH, DEFAULT_AGENT_MAX_THREADS,
};

fn resolve_sqlite_home_env(resolved_cwd: &Path) -> Option<PathBuf> {
    let path = PathBuf::from(chaos_proc::sqlite_home_env_value()?);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(resolved_cwd.join(path))
    }
}

fn add_additional_file_system_writes(
    file_system_sandbox_policy: &mut FileSystemSandboxPolicy,
    additional_writable_roots: &[AbsolutePathBuf],
) {
    for path in additional_writable_roots {
        let exists = file_system_sandbox_policy.entries.iter().any(|entry| {
            matches!(
                &entry.path,
                chaos_ipc::permissions::FileSystemPath::Path { path: existing }
                    if existing == path && entry.access == chaos_ipc::permissions::FileSystemAccessMode::Write
            )
        });
        if !exists {
            file_system_sandbox_policy.entries.push(
                chaos_ipc::permissions::FileSystemSandboxEntry {
                    path: chaos_ipc::permissions::FileSystemPath::Path { path: path.clone() },
                    access: chaos_ipc::permissions::FileSystemAccessMode::Write,
                },
            );
        }
    }
}

#[cfg(test)]
pub(crate) fn resolve_web_search_mode(
    config_toml: &ConfigToml,
    config_profile: &crate::config::profile::ConfigProfile,
    features: &crate::features::Features,
) -> Option<WebSearchMode> {
    resolve_web_search_mode_inner(config_toml, config_profile, features)
}

fn resolve_web_search_mode_inner(
    config_toml: &ConfigToml,
    config_profile: &crate::config::profile::ConfigProfile,
    _features: &crate::features::Features,
) -> Option<WebSearchMode> {
    if let Some(mode) = config_profile.web_search.or(config_toml.web_search) {
        return Some(mode);
    }
    None
}

fn resolve_web_search_config_inner(
    config_toml: &ConfigToml,
    config_profile: &crate::config::profile::ConfigProfile,
) -> Option<chaos_ipc::config_types::WebSearchConfig> {
    let base = config_toml
        .tools
        .as_ref()
        .and_then(|tools| tools.web_search.as_ref());
    let profile = config_profile
        .tools
        .as_ref()
        .and_then(|tools| tools.web_search.as_ref());

    match (base, profile) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone().into()),
        (None, Some(profile)) => Some(profile.clone().into()),
        (Some(base), Some(profile)) => Some(base.merge(profile).into()),
    }
}

impl Config {
    pub(crate) fn reload_mcp_servers_from_layer_stack(&mut self) -> std::io::Result<()> {
        let effective = self.config_layer_stack.effective_config();
        let cfg: ConfigToml = effective.try_into().map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to parse effective config while reloading MCP servers: {err}"),
            )
        })?;
        self.mcp_servers = validation::constrain_mcp_servers(
            cfg.mcp_servers,
            self.config_layer_stack.requirements().mcp_servers.as_ref(),
        )
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn load_from_base_config_with_overrides(
        cfg: ConfigToml,
        overrides: ConfigOverrides,
        chaos_home: PathBuf,
    ) -> std::io::Result<Self> {
        let config_layer_stack = ConfigLayerStack::default();
        Self::load_config_with_layer_stack(cfg, overrides, chaos_home, config_layer_stack)
    }

    pub(crate) fn load_config_with_layer_stack(
        cfg: ConfigToml,
        overrides: ConfigOverrides,
        chaos_home: PathBuf,
        config_layer_stack: ConfigLayerStack,
    ) -> std::io::Result<Self> {
        let ConfigRequirements {
            approval_policy: mut constrained_approval_policy,
            sandbox_policy: mut constrained_sandbox_policy,
            web_search_mode: mut constrained_web_search_mode,
            feature_requirements,
            mcp_servers,
            exec_policy: _,
            enforce_residency,
            network: network_requirements,
        } = config_layer_stack.requirements().clone();

        let user_instructions: Option<String> = None;
        let mut startup_warnings = Vec::new();

        let ConfigOverrides {
            model,
            review_model: override_review_model,
            cwd,
            approval_policy: approval_policy_override,
            approvals_reviewer: approvals_reviewer_override,
            sandbox_mode,
            model_provider,
            service_tier: service_tier_override,
            config_profile: config_profile_key,
            alcatraz_linux_exe,
            alcatraz_freebsd_exe,
            alcatraz_macos_exe,
            base_instructions,
            minion_instructions,
            personality,
            compact_prompt,
            show_raw_agent_reasoning,
            ephemeral,
            additional_writable_roots,
        } = overrides;

        let active_profile_name = config_profile_key
            .as_ref()
            .or(cfg.profile.as_ref())
            .cloned();
        let config_profile = match active_profile_name.as_ref() {
            Some(key) => cfg
                .profiles
                .get(key)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("config profile `{key}` not found"),
                    )
                })?
                .clone(),
            None => crate::config::profile::ConfigProfile::default(),
        };
        let feature_overrides = FeatureOverrides {};

        let configured_features =
            crate::features::features_from_config(&cfg, &config_profile, feature_overrides);
        let features = ManagedFeatures::from_configured(configured_features, feature_requirements)?;
        let resolved_cwd = normalize_for_native_workdir({
            use std::env;

            match cwd {
                None => {
                    tracing::info!("cwd not set, using current dir");
                    env::current_dir()?
                }
                Some(p) if p.is_absolute() => p,
                Some(p) => {
                    tracing::info!("cwd is relative, resolving against current dir");
                    let mut current = env::current_dir()?;
                    current.push(p);
                    current
                }
            }
        });
        let additional_writable_roots: Vec<AbsolutePathBuf> = additional_writable_roots
            .into_iter()
            .map(|path| AbsolutePathBuf::resolve_path_against_base(path, &resolved_cwd))
            .collect::<Result<Vec<_>, _>>()?;
        let active_project = cfg
            .get_active_project(&resolved_cwd)
            .unwrap_or(ProjectConfig { trust_level: None });
        let permission_config_syntax = validation::resolve_permission_config_syntax(
            &config_layer_stack,
            &cfg,
            sandbox_mode,
            config_profile.sandbox_mode,
        );
        let has_permission_profiles = cfg
            .permissions
            .as_ref()
            .is_some_and(|profiles| !profiles.is_empty());
        if has_permission_profiles
            && !matches!(
                permission_config_syntax,
                Some(PermissionConfigSyntax::Legacy)
            )
            && cfg.default_permissions.is_none()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "config defines `[permissions]` profiles but does not set `default_permissions`",
            ));
        }

        let profiles_are_active = matches!(
            permission_config_syntax,
            Some(PermissionConfigSyntax::Profiles)
        ) || (permission_config_syntax.is_none()
            && has_permission_profiles);
        let (
            configured_network_proxy_config,
            sandbox_policy,
            file_system_sandbox_policy,
            network_sandbox_policy,
        ) = if profiles_are_active {
            let permissions = cfg.permissions.as_ref().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "default_permissions requires a `[permissions]` table",
                )
            })?;
            let default_permissions = cfg.default_permissions.as_deref().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "default_permissions requires a named permissions profile",
                )
            })?;
            let profile = resolve_permission_profile(permissions, default_permissions)?;
            let configured_network_proxy_config =
                network_proxy_config_from_profile_network(profile.network.as_ref());
            let (mut file_system_sandbox_policy, network_sandbox_policy) =
                compile_permission_profile(
                    permissions,
                    default_permissions,
                    &mut startup_warnings,
                )?;
            let mut sandbox_policy = file_system_sandbox_policy
                .to_legacy_sandbox_policy(network_sandbox_policy, &resolved_cwd)?;
            if matches!(sandbox_policy, SandboxPolicy::WorkspaceWrite { .. }) {
                add_additional_file_system_writes(
                    &mut file_system_sandbox_policy,
                    &additional_writable_roots,
                );
                sandbox_policy = file_system_sandbox_policy
                    .to_legacy_sandbox_policy(network_sandbox_policy, &resolved_cwd)?;
            }
            (
                configured_network_proxy_config,
                sandbox_policy,
                file_system_sandbox_policy,
                network_sandbox_policy,
            )
        } else {
            let configured_network_proxy_config = NetworkProxyConfig::default();
            let mut sandbox_policy = cfg.derive_sandbox_policy(
                sandbox_mode,
                config_profile.sandbox_mode,
                &resolved_cwd,
                Some(&constrained_sandbox_policy),
            );
            if let SandboxPolicy::WorkspaceWrite { writable_roots, .. } = &mut sandbox_policy {
                for path in &additional_writable_roots {
                    if !writable_roots.iter().any(|existing| existing == path) {
                        writable_roots.push(path.clone());
                    }
                }
            }
            let file_system_sandbox_policy =
                FileSystemSandboxPolicy::from_legacy_sandbox_policy(&sandbox_policy, &resolved_cwd);
            let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);
            (
                configured_network_proxy_config,
                sandbox_policy,
                file_system_sandbox_policy,
                network_sandbox_policy,
            )
        };
        let approval_policy_was_explicit = approval_policy_override.is_some()
            || config_profile.approval_policy.is_some()
            || cfg.approval_policy.is_some();
        let mut approval_policy = approval_policy_override
            .or(config_profile.approval_policy)
            .or(cfg.approval_policy)
            .unwrap_or_else(|| {
                if active_project.is_trusted() {
                    ApprovalPolicy::Interactive
                } else if active_project.is_untrusted() {
                    ApprovalPolicy::Supervised
                } else {
                    ApprovalPolicy::default()
                }
            });
        if !approval_policy_was_explicit
            && let Err(err) = constrained_approval_policy.can_set(&approval_policy)
        {
            tracing::warn!(
                error = %err,
                "default approval policy is disallowed by requirements; falling back to required default"
            );
            approval_policy = constrained_approval_policy.value();
        }
        let approvals_reviewer = approvals_reviewer_override
            .or(config_profile.approvals_reviewer)
            .or(cfg.approvals_reviewer)
            .unwrap_or(ApprovalsReviewer::User);
        let web_search_mode = resolve_web_search_mode_inner(&cfg, &config_profile, &features)
            .unwrap_or(WebSearchMode::Cached);
        let web_search_config = resolve_web_search_config_inner(&cfg, &config_profile);

        let agent_roles = agent_roles::load_agent_roles(
            cfg.agents.as_ref(),
            &config_layer_stack,
            &mut startup_warnings,
        )?;

        let mut model_providers = built_in_model_providers();
        // User-configured providers replace built-ins. Partial overrides
        // against a built-in ID were already merged onto the built-in
        // baseline inside `deserialize_model_providers`, so every entry
        // in `cfg.model_providers` is a complete, ready-to-use provider.
        for (key, provider) in cfg.model_providers.into_iter() {
            model_providers.insert(key, provider);
        }

        let model_provider_id = model_provider
            .or(config_profile.model_provider)
            .or(cfg.model_provider)
            .unwrap_or_else(|| "openai".to_string());
        let model_provider = model_providers
            .get(&model_provider_id)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Model provider `{model_provider_id}` not found"),
                )
            })?
            .clone();

        let shell_environment_policy = cfg.shell_environment_policy.into();
        let allow_login_shell = cfg.allow_login_shell.unwrap_or(true);

        let history = cfg.history.unwrap_or_default();

        let agent_max_threads = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.max_threads)
            .or(DEFAULT_AGENT_MAX_THREADS);
        if agent_max_threads == Some(0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.max_threads must be at least 1",
            ));
        }
        let agent_max_depth = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.max_depth)
            .unwrap_or(DEFAULT_AGENT_MAX_DEPTH);
        if agent_max_depth < 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.max_depth must be at least 1",
            ));
        }
        let agent_job_max_runtime_seconds = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.job_max_runtime_seconds)
            .or(DEFAULT_AGENT_JOB_MAX_RUNTIME_SECONDS);
        if agent_job_max_runtime_seconds == Some(0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.job_max_runtime_seconds must be at least 1",
            ));
        }
        if let Some(max_runtime_seconds) = agent_job_max_runtime_seconds
            && max_runtime_seconds > i64::MAX as u64
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.job_max_runtime_seconds must fit within a 64-bit signed integer",
            ));
        }
        let background_terminal_max_timeout = cfg
            .background_terminal_max_timeout
            .unwrap_or(crate::unified_exec::DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS)
            .max(crate::unified_exec::MIN_EMPTY_YIELD_TIME_MS);

        let ghost_snapshot = {
            let mut config = GhostSnapshotConfig::default();
            if let Some(ghost_snapshot) = cfg.ghost_snapshot.as_ref()
                && let Some(ignore_over_bytes) = ghost_snapshot.ignore_large_untracked_files
            {
                config.ignore_large_untracked_files = if ignore_over_bytes > 0 {
                    Some(ignore_over_bytes)
                } else {
                    None
                };
            }
            if let Some(ghost_snapshot) = cfg.ghost_snapshot.as_ref()
                && let Some(threshold) = ghost_snapshot.ignore_large_untracked_dirs
            {
                config.ignore_large_untracked_dirs =
                    if threshold > 0 { Some(threshold) } else { None };
            }
            if let Some(ghost_snapshot) = cfg.ghost_snapshot.as_ref()
                && let Some(disable_warnings) = ghost_snapshot.disable_warnings
            {
                config.disable_warnings = disable_warnings;
            }
            config
        };

        let forced_chatgpt_workspace_id =
            cfg.forced_chatgpt_workspace_id.as_ref().and_then(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            });

        let forced_login_method = cfg.forced_login_method;

        let model = model.or(config_profile.model).or(cfg.model);
        let service_tier = service_tier_override
            .unwrap_or_else(|| config_profile.service_tier.or(cfg.service_tier));
        let service_tier = match service_tier {
            Some(ServiceTier::Flex) => Some(ServiceTier::Flex),
            _ => None,
        };

        let compact_prompt = compact_prompt.or(cfg.compact_prompt).and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let model_instructions_path = config_profile
            .model_instructions_file
            .as_ref()
            .or(cfg.model_instructions_file.as_ref());
        let file_base_instructions =
            Self::try_read_non_empty_file(model_instructions_path, "model instructions file")?;
        let base_instructions = base_instructions.or(file_base_instructions);
        let minion_instructions = minion_instructions.or(cfg.minion_instructions);
        let personality = personality
            .or(config_profile.personality)
            .or(cfg.personality)
            .or(Some(chaos_ipc::config_types::Personality::Pragmatic));

        let experimental_compact_prompt_path = config_profile
            .experimental_compact_prompt_file
            .as_ref()
            .or(cfg.experimental_compact_prompt_file.as_ref());
        let file_compact_prompt = Self::try_read_non_empty_file(
            experimental_compact_prompt_path,
            "experimental compact prompt file",
        )?;
        let compact_prompt = compact_prompt.or(file_compact_prompt);

        let review_model = override_review_model.or(cfg.review_model);

        let model_catalog = parsing::load_model_catalog(
            config_profile
                .model_catalog_json
                .clone()
                .or(cfg.model_catalog_json.clone()),
        )?;

        let log_dir = cfg
            .log_dir
            .as_ref()
            .map(AbsolutePathBuf::to_path_buf)
            .unwrap_or_else(|| {
                let mut p = chaos_home.clone();
                p.push("log");
                p
            });
        let sqlite_home = cfg
            .sqlite_home
            .as_ref()
            .map(AbsolutePathBuf::to_path_buf)
            .or_else(|| resolve_sqlite_home_env(&resolved_cwd))
            .unwrap_or_else(|| chaos_home.to_path_buf());
        let original_sandbox_policy = sandbox_policy.clone();

        validation::apply_requirement_constrained_value(
            "approval_policy",
            approval_policy,
            &mut constrained_approval_policy,
            &mut startup_warnings,
        )?;
        validation::apply_requirement_constrained_value(
            "sandbox_mode",
            sandbox_policy,
            &mut constrained_sandbox_policy,
            &mut startup_warnings,
        )?;
        validation::apply_requirement_constrained_value(
            "web_search_mode",
            web_search_mode,
            &mut constrained_web_search_mode,
            &mut startup_warnings,
        )?;

        let mcp_servers =
            validation::constrain_mcp_servers(cfg.mcp_servers.clone(), mcp_servers.as_ref())
                .map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{e}"))
                })?;

        let (network_requirements, network_requirements_source) = match network_requirements {
            Some(Sourced { value, source }) => (Some(value), Some(source)),
            None => (None, None),
        };
        let has_network_requirements = network_requirements.is_some();
        let network = NetworkProxySpec::from_config_and_constraints(
            configured_network_proxy_config,
            network_requirements,
            constrained_sandbox_policy.get(),
        )
        .map_err(|err| {
            if let Some(source) = network_requirements_source.as_ref() {
                std::io::Error::new(
                    err.kind(),
                    format!("failed to build managed network proxy from {source}: {err}"),
                )
            } else {
                err
            }
        })?;
        let network = if has_network_requirements {
            Some(network)
        } else {
            network.enabled().then_some(network)
        };
        let effective_sandbox_policy = constrained_sandbox_policy.value.get().clone();
        let effective_file_system_sandbox_policy =
            if effective_sandbox_policy == original_sandbox_policy {
                file_system_sandbox_policy
            } else {
                FileSystemSandboxPolicy::from_legacy_sandbox_policy(
                    &effective_sandbox_policy,
                    &resolved_cwd,
                )
            };
        let effective_network_sandbox_policy =
            if effective_sandbox_policy == original_sandbox_policy {
                network_sandbox_policy
            } else {
                NetworkSandboxPolicy::from(&effective_sandbox_policy)
            };

        let config = Self {
            model,
            service_tier,
            review_model,
            model_context_window: cfg.model_context_window,
            model_auto_compact_token_limit: cfg.model_auto_compact_token_limit,
            model_provider_id,
            model_provider,
            cwd: resolved_cwd,
            startup_warnings,
            permissions: Permissions {
                approval_policy: constrained_approval_policy.value,
                sandbox_policy: constrained_sandbox_policy.value,
                file_system_sandbox_policy: effective_file_system_sandbox_policy,
                network_sandbox_policy: effective_network_sandbox_policy,
                network,
                allow_login_shell,
                shell_environment_policy,
                macos_seatbelt_profile_extensions: None,
            },
            approvals_reviewer,
            enforce_residency: enforce_residency.value,
            notify: cfg.notify,
            user_instructions,
            base_instructions,
            personality,
            minion_instructions,
            compact_prompt,
            cli_auth_credentials_store_mode: cfg.cli_auth_credentials_store.unwrap_or_default(),
            mcp_servers,
            mcp_oauth_credentials_store_mode: cfg.mcp_oauth_credentials_store.unwrap_or_default(),
            mcp_oauth_callback_port: cfg.mcp_oauth_callback_port,
            mcp_oauth_callback_url: cfg.mcp_oauth_callback_url.clone(),
            model_providers,
            tool_output_token_limit: cfg.tool_output_token_limit,
            agent_max_threads,
            agent_max_depth,
            agent_roles,
            agent_job_max_runtime_seconds,
            chaos_home,
            sqlite_home,
            log_dir,
            config_layer_stack,
            history,
            ephemeral: ephemeral.unwrap_or_default(),
            file_opener: cfg.file_opener.unwrap_or(UriBasedFileOpener::VsCode),
            alcatraz_linux_exe,
            alcatraz_freebsd_exe,
            alcatraz_macos_exe,

            hide_agent_reasoning: cfg.hide_agent_reasoning.unwrap_or(false),
            show_raw_agent_reasoning: cfg
                .show_raw_agent_reasoning
                .or(show_raw_agent_reasoning)
                .unwrap_or(false),
            model_reasoning_effort: config_profile
                .model_reasoning_effort
                .or(cfg.model_reasoning_effort),
            plan_mode_reasoning_effort: config_profile
                .plan_mode_reasoning_effort
                .or(cfg.plan_mode_reasoning_effort),
            model_reasoning_summary: config_profile
                .model_reasoning_summary
                .or(cfg.model_reasoning_summary),
            model_supports_reasoning_summaries: cfg.model_supports_reasoning_summaries,
            model_catalog,
            model_verbosity: config_profile.model_verbosity.or(cfg.model_verbosity),
            chatgpt_base_url: config_profile
                .chatgpt_base_url
                .or(cfg.chatgpt_base_url)
                .unwrap_or("https://chatgpt.com/backend-api/".to_string()),
            realtime_audio: cfg
                .audio
                .map_or_else(RealtimeAudioConfig::default, |audio| RealtimeAudioConfig {
                    microphone: audio.microphone,
                    speaker: audio.speaker,
                }),
            experimental_realtime_ws_base_url: cfg.experimental_realtime_ws_base_url,
            experimental_realtime_ws_model: cfg.experimental_realtime_ws_model,
            realtime: cfg
                .realtime
                .map_or_else(RealtimeConfig::default, |realtime| RealtimeConfig {
                    version: realtime.version.unwrap_or_default(),
                    session_type: realtime.session_type.unwrap_or_default(),
                }),
            experimental_realtime_ws_backend_prompt: cfg.experimental_realtime_ws_backend_prompt,
            experimental_realtime_ws_startup_context: cfg.experimental_realtime_ws_startup_context,
            experimental_realtime_start_instructions: cfg.experimental_realtime_start_instructions,
            forced_chatgpt_workspace_id,
            forced_login_method,
            web_search_mode: constrained_web_search_mode.value,
            web_search_config,
            collab_enabled: true,
            background_terminal_max_timeout,
            ghost_snapshot,
            features,
            active_profile: active_profile_name,
            active_project,
            notices: cfg.notice.unwrap_or_default(),
            disable_paste_burst: cfg.disable_paste_burst.unwrap_or(false),
            analytics_enabled: config_profile
                .analytics
                .as_ref()
                .and_then(|a| a.enabled)
                .or(cfg.analytics.as_ref().and_then(|a| a.enabled)),
            feedback_enabled: cfg
                .feedback
                .as_ref()
                .and_then(|feedback| feedback.enabled)
                .unwrap_or(true),
            tui_notifications: cfg
                .tui
                .as_ref()
                .map(|t| t.notifications.clone())
                .unwrap_or_default(),
            tui_notification_method: cfg
                .tui
                .as_ref()
                .map(|t| t.notification_method)
                .unwrap_or_default(),
            animations: cfg.tui.as_ref().map(|t| t.animations).unwrap_or(true),
            model_availability_nux: cfg
                .tui
                .as_ref()
                .map(|t| t.model_availability_nux.clone())
                .unwrap_or_default(),
            tui_alternate_screen: cfg
                .tui
                .as_ref()
                .map(|t| t.alternate_screen)
                .unwrap_or_default(),
            tui_status_line: cfg.tui.as_ref().and_then(|t| t.status_line.clone()),
            tui_theme: cfg.tui.as_ref().and_then(|t| t.theme.clone()),
            otel: {
                let t: OtelConfigToml = cfg.otel.unwrap_or_default();
                let log_user_prompt = t.log_user_prompt.unwrap_or(false);
                let environment = t
                    .environment
                    .unwrap_or(DEFAULT_OTEL_ENVIRONMENT.to_string());
                let exporter = t.exporter.unwrap_or(OtelExporterKind::None);
                let trace_exporter = t.trace_exporter.unwrap_or_else(|| exporter.clone());
                let metrics_exporter = t.metrics_exporter.unwrap_or(OtelExporterKind::None);
                OtelConfig {
                    log_user_prompt,
                    environment,
                    exporter,
                    trace_exporter,
                    metrics_exporter,
                }
            },
            disable_user_scripts: false,
            memories: cfg.memories.map(MemoriesConfig::from).unwrap_or_default(),
        };
        Ok(config)
    }

    /// If `path` is `Some`, reads the file and returns trimmed contents.
    /// Returns `Err` when the file is empty or unreadable.
    pub(crate) fn try_read_non_empty_file(
        path: Option<&AbsolutePathBuf>,
        context: &str,
    ) -> std::io::Result<Option<String>> {
        let Some(path) = path else {
            return Ok(None);
        };

        let contents = std::fs::read_to_string(path).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("failed to read {context} {}: {e}", path.display()),
            )
        })?;

        let s = contents.trim().to_string();
        if s.is_empty() {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{context} is empty: {}", path.display()),
            ))
        } else {
            Ok(Some(s))
        }
    }

    pub fn managed_network_requirements_enabled(&self) -> bool {
        self.config_layer_stack
            .requirements_toml()
            .network
            .is_some()
    }
}
