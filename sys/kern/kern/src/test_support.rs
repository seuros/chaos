//! Test-only helpers exposed for cross-crate integration tests.
//!
//! Production code should not depend on this module.
//! We prefer this to using a crate feature to avoid building multiple
//! permutations of the crate.

use std::path::PathBuf;
use std::sync::Arc;

use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::config_types::Verbosity;
use chaos_ipc::openai_models::ApplyPatchToolType;
use chaos_ipc::openai_models::ConfigShellToolType;
use chaos_ipc::openai_models::InputModality;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ModelPreset;
use chaos_ipc::openai_models::ModelVisibility;
use chaos_ipc::openai_models::ModelsResponse;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::openai_models::ReasoningEffortPreset;
use chaos_ipc::openai_models::TruncationPolicyConfig;
use std::sync::LazyLock;

use crate::AuthManager;
use crate::ChaosAuth;
use crate::ModelProviderInfo;
use crate::ProcessTable;
use crate::config::Config;
use crate::models_manager::collaboration_mode_presets;
use crate::models_manager::manager::ModelsManager;
use crate::process_table;
use crate::unified_exec;

/// Build a provider-agnostic [`ModelInfo`] with sensible defaults for testing.
pub fn test_model_info(slug: &str) -> ModelInfo {
    ModelInfo {
        slug: slug.to_string(),
        display_name: slug.to_string(),
        description: Some(format!("{slug} test model")),
        default_reasoning_level: Some(ReasoningEffort::Medium),
        supported_reasoning_levels: vec![
            ReasoningEffortPreset {
                effort: ReasoningEffort::Low,
                description: "Low".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::Medium,
                description: "Medium".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::High,
                description: "High".to_string(),
            },
        ],
        shell_type: ConfigShellToolType::ShellCommand,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority: 0,
        availability_nux: None,
        base_instructions: format!("You are a test coding assistant ({slug})."),
        model_messages: None,
        supports_reasoning_summaries: false,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: true,
        default_verbosity: Some(Verbosity::Low),
        apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
        web_search_tool_type: Default::default(),
        truncation_policy: TruncationPolicyConfig::tokens(10_000),
        supports_parallel_tool_calls: true,
        supports_image_detail_original: false,
        context_window: Some(272_000),
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities: vec![InputModality::Text, InputModality::Image],
        prefer_websockets: false,
        used_fallback_model_metadata: false,
        supports_search_tool: false,
    }
}

/// Build a [`ModelsResponse`] with models for the given slugs, each with
/// incrementing priority.
pub fn test_models_response(slugs: &[&str]) -> ModelsResponse {
    ModelsResponse {
        models: slugs
            .iter()
            .enumerate()
            .map(|(i, slug)| {
                let mut m = test_model_info(slug);
                m.priority = i as i32;
                m
            })
            .collect(),
    }
}

static TEST_MODEL_PRESETS: LazyLock<Vec<ModelPreset>> = LazyLock::new(|| {
    let response = test_models_response(&["shodan", "cortana"]);
    let mut presets: Vec<ModelPreset> = response.models.into_iter().map(Into::into).collect();
    ModelPreset::mark_default_by_picker_visibility(&mut presets);
    presets
});

pub fn set_process_table_test_mode(enabled: bool) {
    process_table::set_process_table_test_mode_for_tests(enabled);
}

pub fn set_deterministic_process_ids(enabled: bool) {
    unified_exec::set_deterministic_process_ids_for_tests(enabled);
}

pub fn auth_manager_from_auth(auth: ChaosAuth) -> Arc<AuthManager> {
    AuthManager::from_auth_for_testing(auth)
}

pub fn auth_manager_from_auth_with_home(auth: ChaosAuth, chaos_home: PathBuf) -> Arc<AuthManager> {
    AuthManager::from_auth_for_testing_with_home(auth, chaos_home)
}

pub fn process_table_with_models_provider(
    auth: ChaosAuth,
    provider: ModelProviderInfo,
) -> ProcessTable {
    ProcessTable::with_models_provider_for_tests(auth, provider)
}

pub fn process_table_with_models_provider_and_home(
    auth: ChaosAuth,
    provider: ModelProviderInfo,
    chaos_home: PathBuf,
) -> ProcessTable {
    ProcessTable::with_models_provider_and_home_for_tests(auth, provider, chaos_home)
}

pub fn models_manager_with_provider(
    chaos_home: PathBuf,
    auth_manager: Arc<AuthManager>,
    provider: ModelProviderInfo,
) -> ModelsManager {
    ModelsManager::with_provider_for_tests(chaos_home, auth_manager, provider)
}

pub fn get_model_offline(model: Option<&str>) -> String {
    ModelsManager::get_model_offline_for_tests(model)
}

pub fn construct_model_info_offline(model: &str, config: &Config) -> ModelInfo {
    ModelsManager::construct_model_info_offline_for_tests(model, config)
}

pub fn all_model_presets() -> &'static Vec<ModelPreset> {
    &TEST_MODEL_PRESETS
}

pub fn builtin_collaboration_mode_presets() -> Vec<CollaborationModeMask> {
    collaboration_mode_presets::builtin_collaboration_mode_presets(
        collaboration_mode_presets::CollaborationModesConfig::default(),
    )
}

/// RAII guard that sets an environment variable and restores it on drop.
///
/// Use sparingly -- env vars are process-global state. Tests that use this
/// should run serially (e.g. via `#[serial]`).
pub struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    /// Set `key` to `value`, saving the previous value (if any) for restoration.
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: callers are expected to run under `#[serial]` so no
        // concurrent env mutation occurs.
        unsafe {
            std::env::set_var(key, value.as_ref());
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: same serial-test guarantee as `set`.
        unsafe {
            match self.original.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
