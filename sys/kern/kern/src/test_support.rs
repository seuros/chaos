//! Test-only helpers exposed for cross-crate integration tests.
//!
//! Production code should not depend on this module.
//! We prefer this to using a crate feature to avoid building multiple
//! permutations of the crate.

use std::path::PathBuf;
use std::sync::Arc;

use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ModelPreset;
use chaos_ipc::openai_models::ModelsResponse;
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

static TEST_MODEL_PRESETS: LazyLock<Vec<ModelPreset>> = LazyLock::new(|| {
    let file_contents = include_str!("../models.json");
    let mut response: ModelsResponse = serde_json::from_str(file_contents)
        .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"));
    response.models.sort_by(|a, b| a.priority.cmp(&b.priority));
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
