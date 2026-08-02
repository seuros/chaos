use std::collections::HashMap;

use anyhow::Result;
use chaos_ipc::openai_models::ModelVisibility;
use chaos_ipc::openai_models::ModelsResponse;
use chaos_kern::ChaosAuth;
use chaos_kern::ModelProviderInfo;
use chaos_kern::models_manager::manager::RefreshStrategy;
use chaos_kern::test_support::test_remote_model;
use chaos_model_catalog::ModelsCache;
use chaos_model_catalog::ModelsCacheScope;
use chaos_proc::open_runtime_db;
use core_test_support::responses;
use core_test_support::test_chaos::test_chaos;
use jiff::Timestamp;
use pretty_assertions::assert_eq;
use wiremock::MockServer;

const ACTIVE_MODEL: &str = "chaos-test-active";

/// One pass over every case the provider listing has to distinguish: the
/// active provider, a credentialed third party with a cached catalog, a
/// credentialed third party nothing has ever contacted, and a provider with no
/// credentials at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_cached_models_for_every_usable_provider() -> Result<()> {
    let server = MockServer::start().await;
    responses::mount_models_once(
        &server,
        ModelsResponse {
            models: vec![test_remote_model(ACTIVE_MODEL, ModelVisibility::List, 1)],
        },
    )
    .await;

    let test = test_chaos()
        .with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing())
        .build(&server)
        .await?;
    let config = test.config.clone();
    let models_manager = test.process_table.get_models_manager();

    // Populate the active provider's cache the way a normal session would.
    let _ = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;

    let base = config.model_provider.clone();

    let mut cached_third_party = base.clone();
    cached_third_party.name = "Fake Cached".to_string();
    cached_third_party.experimental_bearer_token = Some("bearer-cached".to_string());

    let mut fresh_third_party = base.clone();
    fresh_third_party.name = "Fake Fresh".to_string();
    fresh_third_party.experimental_bearer_token = Some("bearer-fresh".to_string());

    // A second account on the vendor the session is already running: same name,
    // same endpoint, its own id and its own credential.
    let mut second_account = base.clone();
    second_account.experimental_bearer_token = Some("bearer-second".to_string());

    let mut locked_out = base.clone();
    locked_out.name = "Fake Locked".to_string();
    locked_out.experimental_bearer_token = None;
    locked_out.env_key = None;
    locked_out.requires_openai_auth = false;
    locked_out.auth = None;

    // Only the "cached" third party has ever been listed.
    let fetched_at = Timestamp::from_second(1_600_000_000).expect("valid timestamp");
    write_cache(
        config.chaos_home.as_path(),
        &ModelsCache {
            fetched_at,
            etag: None,
            client_version: None,
            scope: Some(scope_for(&cached_third_party)),
            models: vec![
                test_remote_model("cached-fast", ModelVisibility::List, 1),
                test_remote_model("cached-slow", ModelVisibility::List, 2),
            ],
        },
    )
    .await?;

    // Same provider name and wire, different endpoint: account sign-in and an
    // API key serve different catalogs, and only the reachable one counts.
    let mut decoy_scope = scope_for(&cached_third_party);
    // Sorts ahead of the mock server's 127.0.0.1 URL, so a matcher that ignored
    // the endpoint would pick this row first and fail the assertions below.
    decoy_scope.base_url = "http://0.0.0.0:1/v1".to_string();
    write_cache(
        config.chaos_home.as_path(),
        &ModelsCache {
            fetched_at,
            etag: None,
            client_version: None,
            scope: Some(decoy_scope),
            models: vec![test_remote_model("decoy-model", ModelVisibility::List, 1)],
        },
    )
    .await?;

    let providers = HashMap::from([
        ("active".to_string(), base),
        ("cached-third-party".to_string(), cached_third_party),
        ("fresh-third-party".to_string(), fresh_third_party),
        ("locked-out".to_string(), locked_out),
        ("second-account".to_string(), second_account),
    ]);

    let groups = models_manager
        .list_models_by_provider(&providers, "active")
        .await;

    let ids: Vec<&str> = groups.iter().map(|g| g.provider_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "active",
            "cached-third-party",
            "fresh-third-party",
            "second-account"
        ],
        "uncredentialed providers are omitted; the active provider sorts first"
    );

    let active = &groups[0];
    assert!(active.active, "session provider is flagged active");
    assert!(
        active.models.iter().any(|m| m.model == ACTIVE_MODEL),
        "active provider serves its live catalog, got {:?}",
        active.models.iter().map(|m| &m.model).collect::<Vec<_>>()
    );

    let cached = &groups[1];
    assert!(!cached.active);
    assert_eq!(cached.fetched_at, Some(fetched_at));
    assert_eq!(
        cached
            .models
            .iter()
            .map(|m| m.model.as_str())
            .collect::<Vec<_>>(),
        vec!["cached-fast", "cached-slow"],
        "cached catalog is served in priority order, from this provider's endpoint only"
    );

    let fresh = &groups[2];
    assert!(fresh.models.is_empty(), "nothing cached, nothing invented");
    assert_eq!(fresh.fetched_at, None);

    // Same vendor as the session, different credential: it is its own entry and
    // it is not the one in use.
    let second = &groups[3];
    assert!(
        !second.active,
        "only the configured id is active, not every entry naming the same vendor"
    );
    assert!(
        second.models.iter().any(|m| m.model == ACTIVE_MODEL),
        "a second account on the same endpoint serves the same catalog, got {:?}",
        second.models.iter().map(|m| &m.model).collect::<Vec<_>>()
    );

    Ok(())
}

fn scope_for(provider: &ModelProviderInfo) -> ModelsCacheScope {
    ModelsCacheScope {
        provider_name: provider.name.clone(),
        wire_api: provider.wire_api.to_string(),
        base_url: provider
            .base_url
            .clone()
            .unwrap_or_else(|| panic!("test provider should have base_url")),
    }
}

async fn write_cache(sqlite_home: &std::path::Path, cache: &ModelsCache) -> Result<()> {
    let scope = cache
        .scope
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("cache scope expected"))?;
    let pool = open_runtime_db(sqlite_home).await?;
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
    .bind(serde_json::to_string(&cache.models)?)
    .execute(&pool)
    .await?;
    Ok(())
}
