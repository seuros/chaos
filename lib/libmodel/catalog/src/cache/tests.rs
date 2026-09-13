use super::*;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::test]
async fn legacy_raw_rows_are_skipped_while_marked_rows_survive() -> TestResult<()> {
    let (home, manager) = mounted_manager().await?;
    let provider_name = "test-provider".to_string();
    let legacy_scope = ModelsCacheScope {
        provider_name: provider_name.clone(),
        wire_api: "responses".to_string(),
        base_url: "https://legacy.example/v1".to_string(),
    };
    let marked_scope = ModelsCacheScope {
        provider_name: provider_name.clone(),
        wire_api: "responses".to_string(),
        base_url: "https://marked.example/v1".to_string(),
    };
    let client_version = "1.2.3".to_string();
    let legacy_fetched_at = Timestamp::now();
    let marked_fetched_at =
        Timestamp::from_second(legacy_fetched_at.as_second() - 60).expect("valid timestamp");

    let legacy_cache = ModelsCache {
        fetched_at: legacy_fetched_at,
        etag: Some("legacy-etag".to_string()),
        client_version: Some(client_version.clone()),
        scope: Some(legacy_scope.clone()),
        models: vec![
            test_model_info("legacy-picked", "anthropic", true, 99),
            test_model_info("legacy-unused", "anthropic", false, 1),
        ],
    };
    let marked_cache = ModelsCache {
        fetched_at: marked_fetched_at,
        etag: Some("marked-etag".to_string()),
        client_version: Some(client_version.clone()),
        scope: Some(marked_scope.clone()),
        models: vec![
            test_model_info("marked-unused", "anthropic", false, 7),
            test_model_info("marked-picked", "anthropic", true, 11),
        ],
    };

    write_cache_row(&home, &legacy_cache, false).await?;
    write_cache_row(&home, &marked_cache, true).await?;

    assert!(
        manager
            .load_fresh(&client_version, &legacy_scope)
            .await
            .is_none(),
        "legacy raw rows must be treated as cache misses"
    );

    let caches = manager.load_all().await?;
    assert_eq!(caches.len(), 1, "legacy rows must be skipped by load_all");
    assert_eq!(caches[0].scope.as_ref(), Some(&marked_scope));

    let legacy_err = manager.renew_cache_ttl(&legacy_scope).await.unwrap_err();
    assert_eq!(legacy_err.kind(), std::io::ErrorKind::NotFound);

    let marked_before = manager
        .load_fresh(&client_version, &marked_scope)
        .await
        .expect("marked cache should be readable");
    assert_eq!(
        marked_before
            .models
            .iter()
            .map(|model| model.slug.as_str())
            .collect::<Vec<_>>(),
        vec!["marked-unused", "marked-picked"]
    );

    assert_eq!(
        manager.first_model_id(&provider_name).await.as_deref(),
        Some("marked-picked"),
        "first_model_id must skip legacy raw rows"
    );

    manager.renew_cache_ttl(&marked_scope).await?;
    let marked_after = manager
        .load_fresh(&client_version, &marked_scope)
        .await
        .expect("marked cache should stay readable after renewal");
    assert!(
        marked_after.fetched_at > marked_before.fetched_at,
        "renewal should advance fetched_at on marked rows"
    );

    Ok(())
}

#[tokio::test]
async fn shared_decoder_accepts_envelopes_and_rejects_legacy_shapes() -> TestResult<()> {
    let model = test_model_info("decoder-model", "anthropic", true, 1);
    let envelope = encode_models_json(std::slice::from_ref(&model));
    let cases = [
        ("legacy array", serde_json::json!([model.clone()]), None),
        (
            "missing format",
            serde_json::json!({ "models": [model] }),
            None,
        ),
        (
            "unknown format",
            serde_json::json!({ "format": "raw_catalog_v2", "models": [model] }),
            None,
        ),
        ("marked envelope", envelope, Some(vec![model])),
    ];

    for (label, input, expected) in cases {
        let sqlite_json = serde_json::to_string(&input)?;
        assert_eq!(
            decode_models_json_string(sqlite_json),
            expected.clone(),
            "{label} should decode from SQLite string semantics",
        );
        assert_eq!(
            decode_models_json_value(input),
            expected,
            "{label} should decode from Postgres value semantics",
        );
    }

    Ok(())
}

async fn mounted_manager() -> TestResult<(PathBuf, ModelsCacheManager)> {
    let home = unique_home("models-cache-envelope");
    fs::create_dir_all(&home)?;
    let config = chaos_vfs::MountConfig::sqlite_home(&home);
    let backend = chaos_vfs::ChaosVfs::from_config(config.clone()).await?;
    chaos_vfs::mount(config, backend);
    let manager = ModelsCacheManager::new(home.clone(), Duration::from_secs(3_600));
    Ok((home, manager))
}

async fn write_cache_row(
    sqlite_home: &Path,
    cache: &ModelsCache,
    envelope: bool,
) -> TestResult<()> {
    let scope = cache.scope.as_ref().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "cache scope expected")
    })?;
    let pool = match chaos_vfs::pool_for(sqlite_home)? {
        chaos_vfs::Vfs::Sqlite(pool) => pool,
        chaos_vfs::Vfs::Postgres(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "sqlite test expected a sqlite runtime",
            )
            .into());
        }
    };
    let models_json = if envelope {
        serde_json::to_string(&encode_models_json(&cache.models))?
    } else {
        serde_json::to_string(&cache.models)?
    };
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

fn unique_home(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{nonce}-{}", std::process::id()))
}

fn test_model_info(slug: &str, family: &str, supported_in_api: bool, priority: i32) -> ModelInfo {
    serde_json::from_value(serde_json::json!({
        "slug": slug,
        "model_family": family,
        "display_name": format!("{slug} display"),
        "description": format!("{slug} description"),
        "supported_reasoning_levels": [
            { "effort": "medium", "description": "medium" }
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": supported_in_api,
        "priority": priority,
        "base_instructions": "base instructions",
        "supports_reasoning_summaries": false,
        "support_verbosity": false,
        "truncation_policy": { "mode": "bytes", "limit": 10_000 },
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "experimental_supported_tools": [],
    }))
    .expect("valid model info")
}
