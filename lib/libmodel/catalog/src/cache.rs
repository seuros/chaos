use chaos_ipc::openai_models::ModelInfo;
use chaos_vfs::Vfs;
use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;
use sqlx::Row;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use std::io;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;
use tracing::error;
use tracing::info;

const RAW_CATALOG_V1_FORMAT: &str = "raw_catalog_v1";

/// Manages loading and saving of model catalogs in the shared runtime store.
#[derive(Debug)]
pub struct ModelsCacheManager {
    sqlite_home: PathBuf,
    cache_ttl: Duration,
    backend: Option<Vfs>,
}

impl ModelsCacheManager {
    /// Create a new cache manager over the backend serving `sqlite_home`.
    pub fn new(sqlite_home: PathBuf, cache_ttl: Duration) -> Self {
        let backend = match chaos_vfs::pool_for(&sqlite_home) {
            Ok(pool) => Some(pool),
            Err(err) => {
                error!("model cache is unavailable: {err}");
                None
            }
        };
        Self {
            sqlite_home,
            cache_ttl,
            backend,
        }
    }

    /// Where this cache lives, so a caller can build a second manager over the
    /// same store without threading the path alongside it.
    pub fn home(&self) -> &std::path::Path {
        self.sqlite_home.as_path()
    }

    /// Load a fresh, matching, format-marked cache entry.
    pub async fn load_fresh(
        &self,
        expected_version: &str,
        expected_scope: &ModelsCacheScope,
    ) -> Option<ModelsCache> {
        info!(
            storage_hint = %self.sqlite_home.display(),
            expected_version,
            "models cache: attempting load_fresh"
        );
        let cache = match self.load(expected_scope).await {
            Ok(cache) => cache?,
            Err(err) => {
                error!("failed to load models cache: {err}");
                return None;
            }
        };
        info!(
            storage_hint = %self.sqlite_home.display(),
            cached_version = ?cache.client_version,
            fetched_at = %cache.fetched_at,
            "models cache: loaded cache row"
        );
        if cache.client_version.as_deref() != Some(expected_version) {
            info!(
                storage_hint = %self.sqlite_home.display(),
                expected_version,
                cached_version = ?cache.client_version,
                "models cache: cache version mismatch"
            );
            return None;
        }
        if !cache.is_fresh(self.cache_ttl) {
            info!(
                storage_hint = %self.sqlite_home.display(),
                cache_ttl_secs = self.cache_ttl.as_secs(),
                fetched_at = %cache.fetched_at,
                "models cache: cache is stale"
            );
            return None;
        }
        info!(
            storage_hint = %self.sqlite_home.display(),
            cache_ttl_secs = self.cache_ttl.as_secs(),
            "models cache: cache hit"
        );
        Some(cache)
    }

    /// Persist the cache to disk, creating parent directories as needed.
    pub async fn persist_cache(
        &self,
        models: &[ModelInfo],
        etag: Option<String>,
        client_version: String,
        scope: ModelsCacheScope,
    ) {
        let cache = ModelsCache {
            fetched_at: Timestamp::now(),
            etag,
            client_version: Some(client_version),
            scope: Some(scope),
            models: models.to_vec(),
        };
        if let Err(err) = self.save_internal(&cache).await {
            error!("failed to write models cache: {err}");
        }
    }

    /// Renew the cache TTL by updating the fetched_at timestamp to now.
    pub async fn renew_cache_ttl(&self, expected_scope: &ModelsCacheScope) -> io::Result<()> {
        let mut cache = match self.load(expected_scope).await? {
            Some(cache) => cache,
            None => return Err(io::Error::new(ErrorKind::NotFound, "cache not found")),
        };
        cache.fetched_at = Timestamp::now();
        self.save_internal(&cache).await
    }

    /// Load all marked scopes, including stale rows; skip legacy payloads.
    pub async fn load_all(&self) -> io::Result<Vec<ModelsCache>> {
        let Some(pool) = self.runtime_pool().await else {
            return Ok(Vec::new());
        };

        let caches = match pool {
            Vfs::Sqlite(pool) => {
                let rows = sqlx::query(
                    "SELECT provider_name, wire_api, base_url, fetched_at, etag, client_version, models_json \
                     FROM model_catalog_cache \
                     ORDER BY provider_name, wire_api, base_url",
                )
                .fetch_all(&pool)
                .await
                .map_err(io::Error::other)?;
                rows.into_iter()
                    .map(|row| decode_models_cache_row_sqlite(Some(row), None))
                    .collect::<io::Result<Vec<_>>>()?
            }
            Vfs::Postgres(pool) => {
                let rows = sqlx::query(
                    "SELECT provider_name, wire_api, base_url, fetched_at, etag, client_version, models_json \
                     FROM model_catalog_cache \
                     ORDER BY provider_name, wire_api, base_url",
                )
                .fetch_all(&pool)
                .await
                .map_err(io::Error::other)?;
                rows.into_iter()
                    .map(|row| decode_models_cache_row_postgres(Some(row), None))
                    .collect::<io::Result<Vec<_>>>()?
            }
        };

        Ok(caches.into_iter().flatten().collect())
    }

    async fn load(&self, scope: &ModelsCacheScope) -> io::Result<Option<ModelsCache>> {
        let Some(pool) = self.runtime_pool().await else {
            return Ok(None);
        };

        match pool {
            Vfs::Sqlite(pool) => {
                let row = sqlx::query(
                    "SELECT fetched_at, etag, client_version, models_json \
                     FROM model_catalog_cache \
                     WHERE provider_name = ? AND wire_api = ? AND base_url = ?",
                )
                .bind(&scope.provider_name)
                .bind(&scope.wire_api)
                .bind(&scope.base_url)
                .fetch_optional(&pool)
                .await
                .map_err(io::Error::other)?;
                decode_models_cache_row_sqlite(row, Some(scope.clone()))
            }
            Vfs::Postgres(pool) => {
                let row = sqlx::query(
                    "SELECT fetched_at, etag, client_version, models_json \
                     FROM model_catalog_cache \
                     WHERE provider_name = $1 AND wire_api = $2 AND base_url = $3",
                )
                .bind(&scope.provider_name)
                .bind(&scope.wire_api)
                .bind(&scope.base_url)
                .fetch_optional(&pool)
                .await
                .map_err(io::Error::other)?;
                decode_models_cache_row_postgres(row, Some(scope.clone()))
            }
        }
    }

    async fn save_internal(&self, cache: &ModelsCache) -> io::Result<()> {
        let Some(scope) = cache.scope.as_ref() else {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "cache scope is required",
            ));
        };
        let Some(pool) = self.runtime_pool().await else {
            return Err(io::Error::other("runtime db unavailable"));
        };

        match pool {
            Vfs::Sqlite(pool) => {
                let models_json = serde_json::to_string(&encode_models_json(&cache.models))
                    .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;
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
                .await
                .map(|_| ())
                .map_err(io::Error::other)
            }
            Vfs::Postgres(pool) => {
                let models_json = serde_json::to_value(encode_models_json(&cache.models))
                    .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;
                sqlx::query(
                    "INSERT INTO model_catalog_cache \
                        (provider_name, wire_api, base_url, fetched_at, etag, client_version, models_json) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7) \
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
                .await
                .map(|_| ())
                .map_err(io::Error::other)
            }
        }
    }

    async fn runtime_pool(&self) -> Option<Vfs> {
        self.backend.clone()
    }

    /// Return the slug of the highest-priority `supported_in_api` model for
    /// the given provider name, or `None` if the cache is empty or unreachable.
    pub async fn first_model_id(&self, provider_name: &str) -> Option<String> {
        let pool = self.runtime_pool().await?;

        match pool {
            Vfs::Sqlite(pool) => {
                let rows = sqlx::query(
                    "SELECT provider_name, wire_api, base_url, fetched_at, etag, client_version, models_json \
                     FROM model_catalog_cache \
                     WHERE provider_name = ? \
                     ORDER BY fetched_at DESC",
                )
                .bind(provider_name)
                .fetch_all(&pool)
                .await
                .ok()?;
                for row in rows {
                    let Some(cache) = decode_models_cache_row_sqlite(Some(row), None).ok()? else {
                        continue;
                    };
                    if let Some(slug) = first_supported_model_id(&cache.models) {
                        return Some(slug);
                    }
                }
            }
            Vfs::Postgres(pool) => {
                let rows = sqlx::query(
                    "SELECT provider_name, wire_api, base_url, fetched_at, etag, client_version, models_json \
                     FROM model_catalog_cache \
                     WHERE provider_name = $1 \
                     ORDER BY fetched_at DESC",
                )
                .bind(provider_name)
                .fetch_all(&pool)
                .await
                .ok()?;
                for row in rows {
                    let Some(cache) = decode_models_cache_row_postgres(Some(row), None).ok()?
                    else {
                        continue;
                    };
                    if let Some(slug) = first_supported_model_id(&cache.models) {
                        return Some(slug);
                    }
                }
            }
        }

        None
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_ttl(&mut self, ttl: Duration) {
        self.cache_ttl = ttl;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn manipulate_cache_for_test<F>(
        &self,
        scope: &ModelsCacheScope,
        f: F,
    ) -> io::Result<()>
    where
        F: FnOnce(&mut Timestamp),
    {
        self.mutate_cache_for_test(scope, |cache| f(&mut cache.fetched_at))
            .await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn mutate_cache_for_test<F>(&self, scope: &ModelsCacheScope, f: F) -> io::Result<()>
    where
        F: FnOnce(&mut ModelsCache),
    {
        let mut cache = match self.load(scope).await? {
            Some(cache) => cache,
            None => return Err(io::Error::new(ErrorKind::NotFound, "cache not found")),
        };
        f(&mut cache);
        self.save_internal(&cache).await
    }
}

fn encode_models_json(models: &[ModelInfo]) -> serde_json::Value {
    serde_json::json!({
        "format": RAW_CATALOG_V1_FORMAT,
        "models": models,
    })
}

fn decode_models_json_value(models_json: serde_json::Value) -> Option<Vec<ModelInfo>> {
    let format = models_json.get("format")?.as_str()?;
    if format != RAW_CATALOG_V1_FORMAT {
        return None;
    }
    let models = models_json.get("models")?.clone();
    serde_json::from_value(models).ok()
}

fn decode_models_json_string(models_json: String) -> Option<Vec<ModelInfo>> {
    serde_json::from_str::<serde_json::Value>(&models_json)
        .ok()
        .and_then(decode_models_json_value)
}

fn first_supported_model_id(models: &[ModelInfo]) -> Option<String> {
    models
        .iter()
        .filter(|m| m.supported_in_api)
        .max_by_key(|m| m.priority)
        .map(|m| m.slug.clone())
}

fn decode_models_cache_row_sqlite(
    row: Option<SqliteRow>,
    scope_override: Option<ModelsCacheScope>,
) -> io::Result<Option<ModelsCache>> {
    let Some(row) = row else {
        return Ok(None);
    };

    let scope = scope_override.unwrap_or_else(|| ModelsCacheScope {
        provider_name: row.get::<String, _>("provider_name"),
        wire_api: row.get::<String, _>("wire_api"),
        base_url: row.get::<String, _>("base_url"),
    });
    let fetched_at =
        Timestamp::from_second(row.get::<i64, _>("fetched_at")).map_err(io::Error::other)?;
    let Some(models) = decode_models_json_string(row.get::<String, _>("models_json")) else {
        return Ok(None);
    };

    Ok(Some(ModelsCache {
        fetched_at,
        etag: row.get::<Option<String>, _>("etag"),
        client_version: row.get::<Option<String>, _>("client_version"),
        scope: Some(scope),
        models,
    }))
}

fn decode_models_cache_row_postgres(
    row: Option<PgRow>,
    scope_override: Option<ModelsCacheScope>,
) -> io::Result<Option<ModelsCache>> {
    let Some(row) = row else {
        return Ok(None);
    };

    let scope = scope_override.unwrap_or_else(|| ModelsCacheScope {
        provider_name: row.get::<String, _>("provider_name"),
        wire_api: row.get::<String, _>("wire_api"),
        base_url: row.get::<String, _>("base_url"),
    });
    let fetched_at =
        Timestamp::from_second(row.get::<i64, _>("fetched_at")).map_err(io::Error::other)?;
    let Some(models) = decode_models_json_value(row.get::<serde_json::Value, _>("models_json"))
    else {
        return Ok(None);
    };

    Ok(Some(ModelsCache {
        fetched_at,
        etag: row.get::<Option<String>, _>("etag"),
        client_version: row.get::<Option<String>, _>("client_version"),
        scope: Some(scope),
        models,
    }))
}

/// Serialized snapshot of models and metadata cached on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsCache {
    pub fetched_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ModelsCacheScope>,
    pub models: Vec<ModelInfo>,
}

/// Provider identity for a cached model catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelsCacheScope {
    pub provider_name: String,
    pub wire_api: String,
    pub base_url: String,
}

impl ModelsCache {
    fn is_fresh(&self, ttl: Duration) -> bool {
        if ttl.is_zero() {
            return false;
        }
        let age_secs = Timestamp::now().as_second() - self.fetched_at.as_second();
        age_secs >= 0 && (age_secs as u64) <= ttl.as_secs()
    }
}

#[cfg(test)]
mod tests {
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
                serde_json::json!({ "models": [model.clone()] }),
                None,
            ),
            (
                "unknown format",
                serde_json::json!({ "format": "raw_catalog_v2", "models": [model.clone()] }),
                None,
            ),
            ("marked envelope", envelope, Some(vec![model.clone()])),
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

    fn test_model_info(
        slug: &str,
        family: &str,
        supported_in_api: bool,
        priority: i32,
    ) -> ModelInfo {
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
}
