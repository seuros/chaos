use super::*;
use crate::ChaosAuth;
use crate::auth::AuthCredentialsStoreMode;
use crate::auth::login_with_provider_api_key;
use crate::config::ConfigBuilder;
use crate::model_provider_info::WireApi;
use chaos_ipc::openai_models::ModelsResponse;
use core_test_support::responses::mount_models_once;
use jiff::Timestamp;
use jiff::ToSpan;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::tempdir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

/// Build a manager whose model cache lives in its own backend under
/// `chaos_home`, so parallel tests never read each other's rows.
async fn manager_over_own_cache(
    chaos_home: PathBuf,
    auth_manager: Arc<AuthManager>,
    provider: ModelProviderInfo,
) -> ModelsManager {
    let config = chaos_vfs::MountConfig::sqlite_home(&chaos_home);
    if chaos_vfs::mounted(&config).is_none() {
        let backend = chaos_vfs::ChaosVfs::from_config(config.clone())
            .await
            .expect("open test cache backend");
        chaos_vfs::mount(config, backend);
    }
    ModelsManager::with_provider_for_tests(chaos_home, auth_manager, provider)
}

fn remote_model(slug: &str, display: &str, priority: i32) -> ModelInfo {
    remote_model_with_visibility(slug, display, priority, "list")
}

fn remote_model_with_visibility(
    slug: &str,
    display: &str,
    priority: i32,
    visibility: &str,
) -> ModelInfo {
    serde_json::from_value(json!({
            "slug": slug,
            "display_name": display,
            "description": format!("{display} desc"),
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [{"effort": "low", "description": "low"}, {"effort": "medium", "description": "medium"}],
            "shell_type": "shell_command",
            "visibility": visibility,
            "minimal_client_version": [0, 1, 0],
            "supported_in_api": true,
            "priority": priority,
            "upgrade": null,
            "base_instructions": "base instructions",
            "supports_reasoning_summaries": false,
            "support_verbosity": false,
            "default_verbosity": null,
            "apply_patch_tool_type": null,
            "truncation_policy": {"mode": "bytes", "limit": 10_000},
            "supports_parallel_tool_calls": false,
            "supports_image_detail_original": false,
            "context_window": 272_000,
            "experimental_supported_tools": [],
        }))
        .expect("valid model")
}

fn assert_models_contain(actual: &[ModelInfo], expected: &[ModelInfo]) {
    for model in expected {
        assert!(
            actual.iter().any(|candidate| candidate.slug == model.slug),
            "expected model {} in cached list",
            model.slug
        );
    }
}

fn provider_for(base_url: String) -> ModelProviderInfo {
    ModelProviderInfo {
        name: "OpenAI".into(),
        model_family: Default::default(),
        base_url: Some(base_url),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        stream_idle_timeout_ms: Some(5_000),
        requires_openai_auth: false,
        auth: None,
        supports_websockets: false,
        native_server_side_tools: vec![],
    }
}

fn account_provider(name: &str, base_url: &str) -> ModelProviderInfo {
    ModelProviderInfo {
        name: name.to_string(),
        model_family: ModelFamily::new("shared-family"),
        base_url: Some(base_url.to_string()),
        requires_openai_auth: true,
        ..provider_for(base_url.to_string())
    }
}

fn anthropic_provider_for(base_url: String) -> ModelProviderInfo {
    ModelProviderInfo {
        experimental_bearer_token: Some("test-bearer-token".into()),
        ..provider_for(base_url)
    }
}

#[tokio::test]
async fn get_model_info_tracks_fallback_usage() {
    let chaos_home = tempdir().expect("temp dir");
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .build()
        .await
        .expect("load default test config");
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let test_catalog = ModelsResponse {
        models: vec![remote_model("test-model", "Test Model", 1)],
    };
    let manager = ModelsManager::new(
        chaos_home.path().to_path_buf(),
        auth_manager,
        Some(test_catalog),
        CollaborationModesConfig::default(),
    );
    let known_slug = "test-model".to_string();

    let known = manager.get_model_info(known_slug.as_str(), &config).await;
    assert!(!known.used_fallback_model_metadata);
    assert_eq!(known.slug, known_slug);

    let unknown = manager
        .get_model_info("model-that-does-not-exist", &config)
        .await;
    assert!(unknown.used_fallback_model_metadata);
    assert_eq!(unknown.slug, "model-that-does-not-exist");
}

#[tokio::test]
async fn get_model_info_uses_custom_catalog() {
    let chaos_home = tempdir().expect("temp dir");
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .build()
        .await
        .expect("load default test config");
    let mut overlay = remote_model("serpent-overlay", "Overlay", 0);
    overlay.supports_image_detail_original = true;

    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let manager = ModelsManager::new(
        chaos_home.path().to_path_buf(),
        auth_manager,
        Some(ModelsResponse {
            models: vec![overlay],
        }),
        CollaborationModesConfig::default(),
    );

    let model_info = manager
        .get_model_info("serpent-overlay-experiment", &config)
        .await;

    assert_eq!(model_info.slug, "serpent-overlay-experiment");
    assert_eq!(model_info.display_name, "Overlay");
    assert_eq!(model_info.context_window, Some(272_000));
    assert!(model_info.supports_image_detail_original);
    assert!(!model_info.supports_parallel_tool_calls);
    assert!(!model_info.used_fallback_model_metadata);
}

#[tokio::test]
async fn get_model_info_matches_namespaced_suffix() {
    let chaos_home = tempdir().expect("temp dir");
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .build()
        .await
        .expect("load default test config");
    let mut remote = remote_model("sherlock-image", "Image", 0);
    remote.supports_image_detail_original = true;
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let manager = ModelsManager::new(
        chaos_home.path().to_path_buf(),
        auth_manager,
        Some(ModelsResponse {
            models: vec![remote],
        }),
        CollaborationModesConfig::default(),
    );
    let namespaced_model = "custom/sherlock-image".to_string();

    let model_info = manager.get_model_info(&namespaced_model, &config).await;

    assert_eq!(model_info.slug, namespaced_model);
    assert!(model_info.supports_image_detail_original);
    assert!(!model_info.used_fallback_model_metadata);
}

#[tokio::test]
async fn get_model_info_rejects_multi_segment_namespace_suffix_matching() {
    let chaos_home = tempdir().expect("temp dir");
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .build()
        .await
        .expect("load default test config");
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let test_catalog = ModelsResponse {
        models: vec![remote_model("test-model", "Test Model", 1)],
    };
    let manager = ModelsManager::new(
        chaos_home.path().to_path_buf(),
        auth_manager,
        Some(test_catalog),
        CollaborationModesConfig::default(),
    );
    let known_slug = "test-model".to_string();
    let namespaced_model = format!("ns1/ns2/{known_slug}");

    let model_info = manager.get_model_info(&namespaced_model, &config).await;

    assert_eq!(model_info.slug, namespaced_model);
    assert!(model_info.used_fallback_model_metadata);
}

#[tokio::test]
async fn refresh_available_models_sorts_by_priority() {
    let server = MockServer::start().await;
    let remote_models = vec![
        remote_model("priority-low", "Low", 1),
        remote_model("priority-high", "High", 0),
    ];
    let models_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: remote_models.clone(),
        },
    )
    .await;

    let chaos_home = tempdir().expect("temp dir");
    let auth_manager =
        AuthManager::from_auth_for_testing(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let provider = provider_for(server.uri());
    let manager =
        manager_over_own_cache(chaos_home.path().to_path_buf(), auth_manager, provider).await;

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("refresh succeeds");
    let cached_remote = manager.get_remote_models().await;
    assert_models_contain(&cached_remote, &remote_models);

    let available = manager.list_models(RefreshStrategy::OnlineIfUncached).await;
    let high_idx = available
        .iter()
        .position(|model| model.model == "priority-high")
        .expect("priority-high should be listed");
    let low_idx = available
        .iter()
        .position(|model| model.model == "priority-low")
        .expect("priority-low should be listed");
    assert!(
        high_idx < low_idx,
        "higher priority should be listed before lower priority"
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "expected a single /models request"
    );
}

#[tokio::test]
async fn refresh_available_models_uses_cache_when_fresh() {
    let server = MockServer::start().await;
    let remote_models = vec![remote_model("cached", "Cached", 5)];
    let models_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: remote_models.clone(),
        },
    )
    .await;

    let chaos_home = tempdir().expect("temp dir");
    let auth_manager =
        AuthManager::from_auth_for_testing(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let provider = provider_for(server.uri());
    let manager =
        manager_over_own_cache(chaos_home.path().to_path_buf(), auth_manager, provider).await;

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("first refresh succeeds");
    assert_models_contain(&manager.get_remote_models().await, &remote_models);

    // Second call should read from cache and avoid the network.
    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("cached refresh succeeds");
    assert_models_contain(&manager.get_remote_models().await, &remote_models);
    assert_eq!(
        models_mock.requests().len(),
        1,
        "cache hit should avoid a second /models request"
    );
}

#[tokio::test]
async fn refresh_available_models_refetches_when_cache_stale() {
    let server = MockServer::start().await;
    let initial_models = vec![remote_model("stale", "Stale", 1)];
    let initial_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: initial_models.clone(),
        },
    )
    .await;

    let chaos_home = tempdir().expect("temp dir");
    let auth_manager =
        AuthManager::from_auth_for_testing(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let provider = provider_for(server.uri());
    let manager =
        manager_over_own_cache(chaos_home.path().to_path_buf(), auth_manager, provider).await;

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("initial refresh succeeds");

    // Rewrite cache with an old timestamp so it is treated as stale.
    manager
        .cache_manager
        .manipulate_cache_for_test(&manager.cache_scope(), |fetched_at| {
            *fetched_at = Timestamp::now().checked_sub(1.hours()).unwrap();
        })
        .await
        .expect("cache manipulation succeeds");

    let updated_models = vec![remote_model("fresh", "Fresh", 9)];
    server.reset().await;
    let refreshed_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: updated_models.clone(),
        },
    )
    .await;

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("second refresh succeeds");
    assert_models_contain(&manager.get_remote_models().await, &updated_models);
    assert_eq!(
        initial_mock.requests().len(),
        1,
        "initial refresh should only hit /models once"
    );
    assert_eq!(
        refreshed_mock.requests().len(),
        1,
        "stale cache refresh should fetch /models once"
    );
}

#[tokio::test]
async fn refresh_available_models_refetches_when_version_mismatch() {
    let server = MockServer::start().await;
    let initial_models = vec![remote_model("old", "Old", 1)];
    let initial_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: initial_models.clone(),
        },
    )
    .await;

    let chaos_home = tempdir().expect("temp dir");
    let auth_manager =
        AuthManager::from_auth_for_testing(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let provider = provider_for(server.uri());
    let manager =
        manager_over_own_cache(chaos_home.path().to_path_buf(), auth_manager, provider).await;

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("initial refresh succeeds");

    manager
        .cache_manager
        .mutate_cache_for_test(&manager.cache_scope(), |cache| {
            let client_version = crate::models_manager::client_version_to_whole();
            cache.client_version = Some(format!("{client_version}-mismatch"));
        })
        .await
        .expect("cache mutation succeeds");

    let updated_models = vec![remote_model("new", "New", 2)];
    server.reset().await;
    let refreshed_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: updated_models.clone(),
        },
    )
    .await;

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("second refresh succeeds");
    assert_models_contain(&manager.get_remote_models().await, &updated_models);
    assert_eq!(
        initial_mock.requests().len(),
        1,
        "initial refresh should only hit /models once"
    );
    assert_eq!(
        refreshed_mock.requests().len(),
        1,
        "version mismatch should fetch /models once"
    );
}

#[tokio::test]
async fn refresh_available_models_refetches_when_provider_scope_changes() {
    let server_a = MockServer::start().await;
    let server_b = MockServer::start().await;
    let initial_models = vec![remote_model("from-a", "From A", 1)];
    let updated_models = vec![remote_model("from-b", "From B", 2)];
    let initial_mock = mount_models_once(
        &server_a,
        ModelsResponse {
            models: initial_models.clone(),
        },
    )
    .await;
    let refreshed_mock = mount_models_once(
        &server_b,
        ModelsResponse {
            models: updated_models.clone(),
        },
    )
    .await;

    let chaos_home = tempdir().expect("temp dir");
    let auth_manager =
        AuthManager::from_auth_for_testing(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let manager_a = manager_over_own_cache(
        chaos_home.path().to_path_buf(),
        auth_manager.clone(),
        provider_for(server_a.uri()),
    )
    .await;

    manager_a
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("initial refresh succeeds");
    assert_models_contain(&manager_a.get_remote_models().await, &initial_models);

    let manager_b = manager_over_own_cache(
        chaos_home.path().to_path_buf(),
        auth_manager,
        provider_for(server_b.uri()),
    )
    .await;

    manager_b
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("provider-scoped refresh succeeds");

    assert_models_contain(&manager_b.get_remote_models().await, &updated_models);
    assert_eq!(initial_mock.requests().len(), 1);
    assert_eq!(
        refreshed_mock.requests().len(),
        1,
        "provider scope mismatch should force a fresh /models fetch"
    );
}

#[tokio::test]
async fn refresh_available_models_uses_cache_for_anthropic_provider() {
    let server = MockServer::start().await;
    let response_body = json!({
        "data": [{
            "id": "claude-cache-test",
            "display_name": "Claude Cache Test",
            "max_input_tokens": 200000,
            "max_tokens": 8192,
            "capabilities": {
                "thinking": { "supported": true },
                "image_input": { "supported": true },
                "structured_outputs": { "supported": true },
                "effort": { "supported": true }
            }
        }]
    });
    Mock::given(method("GET"))
        .and(path("/anthropic/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .expect(1)
        .mount(&server)
        .await;

    let chaos_home = tempdir().expect("temp dir");
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let manager = manager_over_own_cache(
        chaos_home.path().to_path_buf(),
        auth_manager,
        anthropic_provider_for(format!("{}/anthropic", server.uri())),
    )
    .await;

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("initial anthropic refresh succeeds");
    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("cached anthropic refresh succeeds");

    let models = manager.get_remote_models().await;
    assert!(
        models.iter().any(|model| model.slug == "claude-cache-test"),
        "expected discovered anthropic model to be cached"
    );
}

#[tokio::test]
async fn unsupported_anthropic_provider_caches_empty_catalog_instead_of_bundled_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/anthropic/models"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    let chaos_home = tempdir().expect("temp dir");
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let manager = manager_over_own_cache(
        chaos_home.path().to_path_buf(),
        auth_manager,
        anthropic_provider_for(format!("{}/anthropic", server.uri())),
    )
    .await;

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("unsupported anthropic refresh should not fail");

    assert!(
        manager.get_remote_models().await.is_empty(),
        "unsupported provider should not inherit bundled OpenAI models"
    );
    assert!(
        manager
            .list_models(RefreshStrategy::OnlineIfUncached)
            .await
            .is_empty(),
        "cached unsupported provider should stay empty until real discovery exists"
    );
}

#[tokio::test]
async fn refresh_available_models_drops_removed_remote_models() {
    let server = MockServer::start().await;
    let initial_models = vec![remote_model("remote-old", "Remote Old", 1)];
    let initial_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: initial_models,
        },
    )
    .await;

    let chaos_home = tempdir().expect("temp dir");
    let auth_manager =
        AuthManager::from_auth_for_testing(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let provider = provider_for(server.uri());
    let mut manager =
        manager_over_own_cache(chaos_home.path().to_path_buf(), auth_manager, provider).await;
    manager.cache_manager.set_ttl(Duration::ZERO);

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("initial refresh succeeds");

    server.reset().await;
    let refreshed_models = vec![remote_model("remote-new", "Remote New", 1)];
    let refreshed_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: refreshed_models,
        },
    )
    .await;

    manager
        .refresh_available_models(RefreshStrategy::OnlineIfUncached)
        .await
        .expect("second refresh succeeds");

    let available = manager
        .try_list_models()
        .expect("models should be available");
    assert!(
        available.iter().any(|preset| preset.model == "remote-new"),
        "new remote model should be listed"
    );
    assert!(
        !available.iter().any(|preset| preset.model == "remote-old"),
        "removed remote model should not be listed"
    );
    assert_eq!(
        initial_mock.requests().len(),
        1,
        "initial refresh should only hit /models once"
    );
    assert_eq!(
        refreshed_mock.requests().len(),
        1,
        "second refresh should only hit /models once"
    );
}

#[tokio::test]
async fn refresh_available_models_fetches_regardless_of_auth_mode() {
    // Chaos fetches models from any provider regardless of auth mode.
    // No more ChatGPT auth gate — the adapter is the source of truth.
    let server = MockServer::start().await;
    let dynamic_slug = "dynamic-model-for-all-auth";
    let models_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model(dynamic_slug, "Any Auth", 1)],
        },
    )
    .await;

    let chaos_home = tempdir().expect("temp dir");
    let auth_manager = Arc::new(AuthManager::new(
        chaos_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    ));
    let provider = provider_for(server.uri());
    let manager =
        manager_over_own_cache(chaos_home.path().to_path_buf(), auth_manager, provider).await;

    manager
        .refresh_available_models(RefreshStrategy::Online)
        .await
        .expect("refresh should fetch from provider");
    let cached_remote = manager.get_remote_models().await;
    assert!(
        cached_remote
            .iter()
            .any(|candidate| candidate.slug == dynamic_slug),
        "models should be fetched regardless of auth mode"
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "provider should be queried for models"
    );
}

#[test]
fn build_available_models_picks_default_after_hiding_hidden_models() {
    let chaos_home = tempdir().expect("temp dir");
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let provider = provider_for("http://example.test".to_string());
    let manager = ModelsManager::with_provider_for_tests(
        chaos_home.path().to_path_buf(),
        auth_manager,
        provider,
    );

    let hidden_model = remote_model_with_visibility("hidden", "Hidden", 0, "hide");
    let visible_model = remote_model_with_visibility("visible", "Visible", 1, "list");

    let expected_hidden = ModelPreset::from(hidden_model.clone());
    let mut expected_visible = ModelPreset::from(visible_model.clone());
    expected_visible.is_default = true;

    let available = manager.build_available_models(vec![hidden_model, visible_model]);

    assert_eq!(available, vec![expected_hidden, expected_visible]);
}

#[test]
fn test_models_response_roundtrips() {
    let response = crate::test_support::test_models_response(&["glados", "shodan", "cortana"]);

    let serialized = serde_json::to_string(&response).expect("test models should serialize");
    let roundtripped: ModelsResponse =
        serde_json::from_str(&serialized).expect("serialized models should deserialize");

    assert_eq!(
        response, roundtripped,
        "test models should round trip through serde"
    );
    assert!(
        !response.models.is_empty(),
        "test models should contain at least one model"
    );
}

/// A manager rebound to another provider resolves models from that provider,
/// not from the one it was constructed with. This is what keeps a process that
/// picked a non-default provider from inheriting a model name the provider it
/// chose has never heard of.
#[tokio::test]
async fn rebound_manager_resolves_models_from_the_new_provider() {
    let chaos_home = tempdir().expect("temp dir");
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));

    let default_server = MockServer::start().await;
    mount_models_once(
        &default_server,
        ModelsResponse {
            models: vec![remote_model("default-provider-model", "Default", 1)],
        },
    )
    .await;
    let chosen_server = MockServer::start().await;
    mount_models_once(
        &chosen_server,
        ModelsResponse {
            models: vec![remote_model("chosen-provider-model", "Chosen", 1)],
        },
    )
    .await;

    let manager = manager_over_own_cache(
        chaos_home.path().to_path_buf(),
        auth_manager,
        provider_for(default_server.uri()),
    )
    .await;

    assert_eq!(
        manager
            .get_default_model(&None, RefreshStrategy::OnlineIfUncached)
            .await,
        "default-provider-model"
    );

    let rebound = manager
        .rebound_to("chosen", provider_for(chosen_server.uri()))
        .expect("a catalog fetched from the network can be rebound");

    assert_eq!(
        rebound
            .get_default_model(&None, RefreshStrategy::OnlineIfUncached)
            .await,
        "chosen-provider-model"
    );
    assert_eq!(
        manager
            .get_default_model(&None, RefreshStrategy::OnlineIfUncached)
            .await,
        "default-provider-model",
        "rebinding hands back a new manager and leaves the original pointed where it was"
    );
}

/// A caller-supplied catalog is authoritative and describes no particular
/// provider, so there is nothing to rebind.
#[tokio::test]
async fn custom_catalog_refuses_to_rebind() {
    let chaos_home = tempdir().expect("temp dir");
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let manager = ModelsManager::new(
        chaos_home.path().to_path_buf(),
        auth_manager,
        Some(ModelsResponse {
            models: vec![remote_model("supplied", "Supplied", 1)],
        }),
        CollaborationModesConfig::default(),
    );

    assert!(
        manager
            .rebound_to("chosen", provider_for("http://127.0.0.1:1/v1".to_string()))
            .is_none()
    );
}

#[tokio::test]
async fn two_provider_account_bindings_use_their_own_cached_catalog_and_subject() {
    let chaos_home = tempdir().expect("temp dir");
    login_with_provider_api_key(
        chaos_home.path(),
        "account-a",
        "secret-a",
        AuthCredentialsStoreMode::File,
    )
    .expect("store account a");
    login_with_provider_api_key(
        chaos_home.path(),
        "account-b",
        "secret-b",
        AuthCredentialsStoreMode::File,
    )
    .expect("store account b");
    let root_auth = AuthManager::shared(
        chaos_home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let provider_a = account_provider("Provider A", "https://a.example.test/v1");
    let provider_b = account_provider("Provider B", "https://b.example.test/v1");
    let manager = manager_over_own_cache(
        chaos_home.path().to_path_buf(),
        root_auth.for_provider("account-a"),
        provider_a.clone(),
    )
    .await;
    manager
        .cache_manager
        .persist_cache(
            &[remote_model("model-a", "Model A", 1)],
            None,
            crate::models_manager::client_version_to_whole(),
            manager.cache_scope(),
        )
        .await;
    let rebound_b = manager
        .rebound_to("account-b", provider_b.clone())
        .expect("default catalog manager can rebind");
    rebound_b
        .cache_manager
        .persist_cache(
            &[remote_model("model-b", "Model B", 1)],
            None,
            crate::models_manager::client_version_to_whole(),
            rebound_b.cache_scope(),
        )
        .await;

    let models_a = manager
        .usable_cached_models_for_provider("account-a", &provider_a)
        .await
        .expect("account a cached catalog");
    let models_b = manager
        .usable_cached_models_for_provider("account-b", &provider_b)
        .await
        .expect("account b cached catalog");
    let subject_a = root_auth
        .credential_subject_fingerprint_for_provider(
            "account-a",
            crate::review_provenance::REVIEW_ACCOUNT_SUBJECT_DOMAIN,
        )
        .expect("account a subject");
    let subject_b = root_auth
        .credential_subject_fingerprint_for_provider(
            "account-b",
            crate::review_provenance::REVIEW_ACCOUNT_SUBJECT_DOMAIN,
        )
        .expect("account b subject");

    assert_eq!(manager.provider_id(), "account-a");
    assert_eq!(rebound_b.provider_id(), "account-b");
    assert_eq!(
        models_a
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["model-a"]
    );
    assert_eq!(
        models_b
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["model-b"]
    );
    assert_ne!(subject_a, subject_b);
    assert!(!subject_a.as_str().contains("secret-a"));
    assert!(!subject_b.as_str().contains("secret-b"));
}

#[tokio::test]
async fn provider_bound_cached_lookup_fails_when_custom_manager_cannot_rebind() {
    let chaos_home = tempdir().expect("temp dir");
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let manager = ModelsManager::new(
        chaos_home.path().to_path_buf(),
        auth_manager,
        Some(ModelsResponse {
            models: vec![remote_model("supplied", "Supplied", 1)],
        }),
        CollaborationModesConfig::default(),
    );
    let mut target = provider_for("http://127.0.0.1:1/v1".to_string());
    target.experimental_bearer_token = Some("configured-but-never-sent".to_string());

    let error = manager
        .usable_cached_models_for_provider("chosen", &target)
        .await
        .expect_err("custom catalog must fail closed instead of falling back");
    assert!(error.to_string().contains("cannot be rebound"));
}

#[tokio::test]
async fn explicit_catalog_family_wins_and_provider_family_only_fills_unknown() {
    let chaos_home = tempdir().expect("temp dir");
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let inherited = remote_model("inherited", "Inherited", 1);
    let mut explicit = remote_model("explicit", "Explicit", 2);
    explicit.model_family = ModelFamily::new("catalog-family");
    let provider = ModelProviderInfo {
        model_family: ModelFamily::new("provider-family"),
        ..provider_for("https://family.example.test/v1".to_string())
    };
    let manager = ModelsManager::new_with_provider_binding(
        chaos_home.path().to_path_buf(),
        auth_manager,
        Some(ModelsResponse {
            models: vec![inherited.clone(), explicit],
        }),
        CollaborationModesConfig::default(),
        "family-provider".to_string(),
        provider,
    );

    let models = manager.list_models(RefreshStrategy::Offline).await;
    let inherited = models
        .iter()
        .find(|model| model.model == inherited.slug)
        .expect("inherited model");
    let explicit = models
        .iter()
        .find(|model| model.model == "explicit")
        .expect("explicit model");
    assert_eq!(inherited.model_family.as_str(), "provider-family");
    assert_eq!(explicit.model_family.as_str(), "catalog-family");
}
