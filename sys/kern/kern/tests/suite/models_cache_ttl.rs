use std::sync::Arc;

use anyhow::Result;
use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::openai_models::ConfigShellToolType;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ModelVisibility;
use chaos_ipc::openai_models::ModelsResponse;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::openai_models::ReasoningEffortPreset;
use chaos_ipc::openai_models::TruncationPolicyConfig;
use chaos_ipc::openai_models::default_input_modalities;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::user_input::UserInput;
use chaos_kern::ChaosAuth;
use chaos_kern::ModelProviderInfo;
use chaos_kern::models_manager::manager::RefreshStrategy;
use chaos_proc::open_chaos_db;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use jiff::Timestamp;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde::Serialize;
use sqlx::Row;
use wiremock::MockServer;

const ETAG: &str = "\"models-etag-ttl\"";
const REMOTE_MODEL: &str = "codex-test-ttl";
const VERSIONED_MODEL: &str = "codex-test-versioned";
const MISSING_VERSION_MODEL: &str = "codex-test-missing-version";
const DIFFERENT_VERSION_MODEL: &str = "codex-test-different-version";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renews_cache_ttl_on_matching_models_etag() -> Result<()> {
    let server = MockServer::start().await;

    let remote_model = test_remote_model(REMOTE_MODEL, 1);
    let models_mock = responses::mount_models_once_with_etag(
        &server,
        ModelsResponse {
            models: vec![remote_model.clone()],
        },
        ETAG,
    )
    .await;

    let mut builder = test_codex().with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    builder = builder.with_config(|config| {
        config.model = Some("gpt-5".to_string());
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(1);
    });

    let test = builder.build(&server).await?;
    let codex = Arc::clone(&test.codex);
    let config = test.config.clone();

    // Populate cache via initial refresh.
    let models_manager = test.process_table.get_models_manager();
    let _ = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;

    let cache_scope = cache_scope_for_provider(&config.model_provider);
    let stale_time = Timestamp::from_second(0).expect("valid epoch");
    rewrite_cache_timestamp(config.chaos_home.as_path(), &cache_scope, stale_time).await?;

    // Trigger responses with matching ETag, which should renew the cache TTL without another /models.
    let response_body = sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-1"),
    ]);
    let _responses_mock = responses::mount_response_once(
        &server,
        sse_response(response_body).insert_header("X-Models-Etag", ETAG),
    )
    .await;

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hi".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: chaos_ipc::protocol::ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::RootAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let _ = wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let refreshed_cache = read_cache(config.chaos_home.as_path(), &cache_scope).await?;
    assert!(
        refreshed_cache.fetched_at > stale_time,
        "cache TTL should be renewed"
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "/models should not refetch on matching etag"
    );

    // Cached models remain usable offline.
    let offline_models = test
        .process_table
        .list_models(RefreshStrategy::Offline)
        .await;
    assert!(
        offline_models
            .iter()
            .any(|preset| preset.model == REMOTE_MODEL),
        "offline listing should use renewed cache"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_cache_when_version_matches() -> Result<()> {
    let server = MockServer::start().await;
    let cached_model = test_remote_model(VERSIONED_MODEL, 1);
    let models_mock = responses::mount_models_once(
        &server,
        ModelsResponse {
            models: vec![test_remote_model("remote", 2)],
        },
    )
    .await;

    let mut builder = test_codex().with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let server_uri = format!("{}/v1", server.uri());
    builder = builder
        .with_pre_build_hook(move |home| {
            let cache = ModelsCache {
                fetched_at: Timestamp::now(),
                etag: None,
                client_version: Some(chaos_kern::models_manager::client_version_to_whole()),
                scope: Some(cache_scope_for_base_url(server_uri.clone())),
                models: vec![cached_model],
            };
            write_cache_sync(home, &cache).expect("write cache");
        })
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
        });

    let test = builder.build(&server).await?;
    let models_manager = test.process_table.get_models_manager();
    let models = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;

    assert!(
        models.iter().any(|preset| preset.model == VERSIONED_MODEL),
        "expected cached model"
    );
    assert_eq!(
        models_mock.requests().len(),
        0,
        "/models should not be called when cache version matches"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_when_cache_version_missing() -> Result<()> {
    let server = MockServer::start().await;
    let cached_model = test_remote_model(MISSING_VERSION_MODEL, 1);
    let models_mock = responses::mount_models_once(
        &server,
        ModelsResponse {
            models: vec![test_remote_model("remote-missing", 2)],
        },
    )
    .await;

    let mut builder = test_codex().with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let server_uri = format!("{}/v1", server.uri());
    builder = builder
        .with_pre_build_hook(move |home| {
            let cache = ModelsCache {
                fetched_at: Timestamp::now(),
                etag: None,
                client_version: None,
                scope: Some(cache_scope_for_base_url(server_uri.clone())),
                models: vec![cached_model],
            };
            write_cache_sync(home, &cache).expect("write cache");
        })
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
        });

    let test = builder.build(&server).await?;
    let models_manager = test.process_table.get_models_manager();
    let models = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;

    assert!(
        models.iter().any(|preset| preset.model == "remote-missing"),
        "expected refreshed models"
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "/models should be called when cache version is missing"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_when_cache_version_differs() -> Result<()> {
    let server = MockServer::start().await;
    let cached_model = test_remote_model(DIFFERENT_VERSION_MODEL, 1);
    let models_response = ModelsResponse {
        models: vec![test_remote_model("remote-different", 2)],
    };
    let mut models_mocks = Vec::new();
    for _ in 0..3 {
        models_mocks.push(responses::mount_models_once(&server, models_response.clone()).await);
    }

    let mut builder = test_codex().with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let server_uri = format!("{}/v1", server.uri());
    builder = builder
        .with_pre_build_hook(move |home| {
            let client_version = chaos_kern::models_manager::client_version_to_whole();
            let cache = ModelsCache {
                fetched_at: Timestamp::now(),
                etag: None,
                client_version: Some(format!("{client_version}-diff")),
                scope: Some(cache_scope_for_base_url(server_uri.clone())),
                models: vec![cached_model],
            };
            write_cache_sync(home, &cache).expect("write cache");
        })
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
        });

    let test = builder.build(&server).await?;
    let models_manager = test.process_table.get_models_manager();
    let models = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;

    assert!(
        models
            .iter()
            .any(|preset| preset.model == "remote-different"),
        "expected refreshed models"
    );
    let models_request_count: usize = models_mocks.iter().map(|mock| mock.requests().len()).sum();
    assert!(
        models_request_count >= 1,
        "/models should be called when cache version differs"
    );

    Ok(())
}

async fn rewrite_cache_timestamp(
    sqlite_home: &std::path::Path,
    scope: &ModelsCacheScope,
    fetched_at: Timestamp,
) -> Result<()> {
    let mut cache = read_cache(sqlite_home, scope).await?;
    cache.fetched_at = fetched_at;
    write_cache(sqlite_home, &cache).await?;
    Ok(())
}

async fn read_cache(
    sqlite_home: &std::path::Path,
    scope: &ModelsCacheScope,
) -> Result<ModelsCache> {
    let pool = open_chaos_db(sqlite_home).await?;
    let row = sqlx::query(
        "SELECT fetched_at, etag, client_version, models_json \
         FROM model_catalog_cache \
         WHERE provider_name = ? AND wire_api = ? AND base_url = ?",
    )
    .bind(&scope.provider_name)
    .bind(&scope.wire_api)
    .bind(&scope.base_url)
    .fetch_one(&pool)
    .await?;
    let models_json = row.get::<String, _>("models_json");
    Ok(ModelsCache {
        fetched_at: Timestamp::from_second(row.get::<i64, _>("fetched_at"))
            .map_err(|_| anyhow::anyhow!("valid timestamp expected"))?,
        etag: row.get::<Option<String>, _>("etag"),
        client_version: row.get::<Option<String>, _>("client_version"),
        scope: Some(scope.clone()),
        models: serde_json::from_str(&models_json)?,
    })
}

async fn write_cache(sqlite_home: &std::path::Path, cache: &ModelsCache) -> Result<()> {
    let scope = cache
        .scope
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("cache scope expected"))?;
    let pool = open_chaos_db(sqlite_home).await?;
    let models_json = serde_json::to_string(&cache.models)?;
    sqlx::query(
        "INSERT INTO model_catalog_cache \
            (provider_name, wire_api, base_url, fetched_at, etag, client_version, models_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(provider_name, wire_api, base_url) DO UPDATE SET \
            fetched_at = excluded.fetched_at, \
            etag = excluded.etag, \
            client_version = excluded.client_version, \
            models_json = excluded.models_json",
    )
    .bind(&scope.provider_name)
    .bind(&scope.wire_api)
    .bind(&scope.base_url)
    .bind(cache.fetched_at.as_second())
    .bind(cache.etag.as_deref())
    .bind(cache.client_version.as_deref())
    .bind(models_json)
    .execute(&pool)
    .await?;
    Ok(())
}

fn write_cache_sync(sqlite_home: &std::path::Path, cache: &ModelsCache) -> Result<()> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(write_cache(sqlite_home, cache))
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelsCache {
    fetched_at: Timestamp,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    client_version: Option<String>,
    #[serde(default)]
    scope: Option<ModelsCacheScope>,
    models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelsCacheScope {
    provider_name: String,
    wire_api: String,
    base_url: String,
}

fn cache_scope_for_provider(provider: &ModelProviderInfo) -> ModelsCacheScope {
    ModelsCacheScope {
        provider_name: provider.name.clone(),
        wire_api: provider.wire_api.to_string(),
        base_url: provider
            .base_url
            .clone()
            .unwrap_or_else(|| panic!("test provider should have base_url")),
    }
}

fn cache_scope_for_base_url(base_url: String) -> ModelsCacheScope {
    ModelsCacheScope {
        provider_name: "OpenAI".to_string(),
        wire_api: "responses".to_string(),
        base_url,
    }
}

fn test_remote_model(slug: &str, priority: i32) -> ModelInfo {
    ModelInfo {
        slug: slug.to_string(),
        display_name: "Remote Test".to_string(),
        description: Some("remote model".to_string()),
        default_reasoning_level: Some(ReasoningEffort::Medium),
        supported_reasoning_levels: vec![
            ReasoningEffortPreset {
                effort: ReasoningEffort::Low,
                description: "low".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::Medium,
                description: "medium".to_string(),
            },
        ],
        shell_type: ConfigShellToolType::ShellCommand,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority,
        base_instructions: "base instructions".to_string(),
        model_messages: None,
        supports_reasoning_summaries: false,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        availability_nux: None,
        apply_patch_tool_type: None,
        web_search_tool_type: Default::default(),
        truncation_policy: TruncationPolicyConfig::bytes(10_000),
        supports_parallel_tool_calls: false,
        supports_image_detail_original: false,
        context_window: Some(272_000),
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities: default_input_modalities(),
        prefer_websockets: false,
        used_fallback_model_metadata: false,
        supports_search_tool: false,
    }
}
