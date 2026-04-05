use chaos_ipc::openai_models::TruncationPolicyConfig;
use chaos_kern::ChaosAuth;
use chaos_kern::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use chaos_kern::models_manager::manager::ModelsManager;
use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_without_tool_output_override() {
    let chaos_home = TempDir::new().expect("create temp dir");
    let config = load_default_config_for_test(&chaos_home).await;
    let auth_manager = chaos_kern::test_support::auth_manager_from_auth(
        ChaosAuth::create_dummy_chatgpt_auth_for_testing(),
    );
    let manager = ModelsManager::new(
        config.chaos_home.clone(),
        auth_manager,
        None,
        CollaborationModesConfig::default(),
    );

    let model_info = manager.get_model_info("gpt-5.1", &config).await;

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::bytes(10_000)
    );
}
