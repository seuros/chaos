use super::*;
use crate::collaboration_modes::CollaborationModesConfig;
use crate::models_manager::manager::RefreshStrategy;
use chaos_ipc::openai_models::ModelsResponse;
use core_test_support::responses::mount_models_once;
use pretty_assertions::assert_eq;
use wiremock::MockServer;

#[tokio::test]
async fn new_uses_configured_openai_provider_for_model_refresh() {
    let server = MockServer::start().await;
    let models_mock = mount_models_once(&server, ModelsResponse { models: vec![] }).await;
    let temp_dir = tempdir().expect("tempdir");
    let mut config = test_config();
    config.chaos_home = temp_dir.path().join("chaos-home");
    config.cwd = config.chaos_home.clone();
    std::fs::create_dir_all(&config.chaos_home).expect("create chaos home");
    config.model_catalog = None;
    config.model_providers.get_mut("openai").unwrap().base_url = Some(server.uri());
    config.model_provider.base_url = Some(server.uri());
    let auth_manager =
        AuthManager::from_auth_for_testing(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let manager = ProcessTable::new(
        &config,
        auth_manager,
        SessionSource::Exec,
        CollaborationModesConfig::default(),
    );
    manager.list_models(RefreshStrategy::Online).await;
    assert_eq!(models_mock.requests().len(), 1);
}

#[tokio::test]
async fn test_process_manager_discovery_is_offline_unless_explicitly_requested() {
    let server = MockServer::start().await;
    let models_mock = mount_models_once(&server, ModelsResponse { models: vec![] }).await;
    let home = tempdir().unwrap();
    crate::test_support::bootstrap_sqlite(home.path()).unwrap();
    let mut provider = test_config().model_provider;
    provider.base_url = Some(server.uri());
    let manager = ProcessTable::with_models_provider_and_home_for_tests(
        ChaosAuth::create_dummy_chatgpt_auth_for_testing(),
        provider,
        home.path().to_path_buf(),
    );
    // These are the implicit lookups used by parent-session initialization.
    for strategy in [RefreshStrategy::OnlineIfUncached, RefreshStrategy::Offline] {
        manager.list_models(strategy).await;
    }
    assert!(
        models_mock.requests().is_empty(),
        "startup must not fetch models"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    manager.list_models(RefreshStrategy::Online).await;
    assert_eq!(
        models_mock.requests().len(),
        1,
        "explicit mock refresh still works"
    );
}
