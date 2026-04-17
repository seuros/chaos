use chaos_ipc::openai_models::ModelInfo;
use chaos_storage::ChaosStorageProvider;
use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;
use sqlx::PgPool;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use std::io;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::OnceCell;
use tracing::error;
use tracing::info;

/// Manages loading and saving of model catalogs in the shared runtime store.
#[derive(Debug)]
pub struct ModelsCacheManager {
    sqlite_home: PathBuf,
    cache_ttl: Duration,
    chaos_pool: OnceCell<Option<RuntimeCachePool>>,
}

#[derive(Debug, Clone)]
enum RuntimeCachePool {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

impl ModelsCacheManager {
    /// Create a new cache manager backed by the shared runtime store.
    pub fn new(sqlite_home: PathBuf, cache_ttl: Duration) -> Self {
        Self {
            sqlite_home,
            cache_ttl,
            chaos_pool: OnceCell::new(),
        }
    }

    /// Attempt to load a fresh cache entry. Returns `None` if the cache doesn't exist, is stale,
    /// or was written for a different provider scope.
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

    async fn load(&self, scope: &ModelsCacheScope) -> io::Result<Option<ModelsCache>> {
        let Some(pool) = self.runtime_pool().await else {
            return Ok(None);
        };

        match pool {
            RuntimeCachePool::Sqlite(pool) => {
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
            RuntimeCachePool::Postgres(pool) => {
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
            RuntimeCachePool::Sqlite(pool) => {
                let models_json = serde_json::to_string(&cache.models)
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
            RuntimeCachePool::Postgres(pool) => {
                let models_json = serde_json::to_value(&cache.models)
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

    async fn runtime_pool(&self) -> Option<RuntimeCachePool> {
        self.chaos_pool
            .get_or_init(|| async {
                match ChaosStorageProvider::from_env(None).await {
                    Ok(provider) => {
                        if let Some(pool) = provider.sqlite_pool_cloned() {
                            return Some(RuntimeCachePool::Sqlite(pool));
                        }
                        if let Some(pool) = provider.postgres_pool_cloned() {
                            return Some(RuntimeCachePool::Postgres(pool));
                        }
                        error!(
                            "failed to resolve supported runtime storage backend for model cache"
                        );
                        None
                    }
                    Err(_) => match ChaosStorageProvider::from_optional_sqlite(
                        None,
                        Some(self.sqlite_home.as_path()),
                    )
                    .await
                    {
                        Ok(provider) => provider.sqlite_pool_cloned().map(RuntimeCachePool::Sqlite),
                        Err(err) => {
                            error!(
                                "failed to open runtime db for model cache at {}: {err}",
                                self.sqlite_home.display()
                            );
                            None
                        }
                    },
                }
            })
            .await
            .clone()
    }

    /// Return the slug of the highest-priority `supported_in_api` model for
    /// the given provider name, or `None` if the cache is empty or unreachable.
    pub async fn first_model_id(&self, provider_name: &str) -> Option<String> {
        let pool = self.runtime_pool().await?;

        let models: Vec<ModelInfo> = match pool {
            RuntimeCachePool::Sqlite(pool) => {
                let json: String = sqlx::query_scalar(
                    "SELECT models_json FROM model_catalog_cache \
                     WHERE provider_name = ? \
                     ORDER BY fetched_at DESC LIMIT 1",
                )
                .bind(provider_name)
                .fetch_optional(&pool)
                .await
                .ok()??;
                serde_json::from_str(&json).ok()?
            }
            RuntimeCachePool::Postgres(pool) => {
                let json: serde_json::Value = sqlx::query_scalar(
                    "SELECT models_json FROM model_catalog_cache \
                     WHERE provider_name = $1 \
                     ORDER BY fetched_at DESC LIMIT 1",
                )
                .bind(provider_name)
                .fetch_optional(&pool)
                .await
                .ok()??;
                serde_json::from_value(json).ok()?
            }
        };

        models
            .into_iter()
            .filter(|m| m.supported_in_api)
            .max_by_key(|m| m.priority)
            .map(|m| m.slug)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_ttl(&mut self, ttl: Duration) {
        self.cache_ttl = ttl;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn manipulate_cache_for_test<F>(&self, f: F) -> io::Result<()>
    where
        F: FnOnce(&mut Timestamp),
    {
        let mut cache = match self.load_first_for_test().await? {
            Some(cache) => cache,
            None => return Err(io::Error::new(ErrorKind::NotFound, "cache not found")),
        };
        f(&mut cache.fetched_at);
        self.save_internal(&cache).await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn mutate_cache_for_test<F>(&self, f: F) -> io::Result<()>
    where
        F: FnOnce(&mut ModelsCache),
    {
        let mut cache = match self.load_first_for_test().await? {
            Some(cache) => cache,
            None => return Err(io::Error::new(ErrorKind::NotFound, "cache not found")),
        };
        f(&mut cache);
        self.save_internal(&cache).await
    }

    #[cfg(any(test, feature = "test-support"))]
    async fn load_first_for_test(&self) -> io::Result<Option<ModelsCache>> {
        let Some(pool) = self.runtime_pool().await else {
            return Ok(None);
        };
        match pool {
            RuntimeCachePool::Sqlite(pool) => {
                let row = sqlx::query(
                    "SELECT provider_name, wire_api, base_url, fetched_at, etag, client_version, models_json \
                     FROM model_catalog_cache \
                     ORDER BY fetched_at DESC \
                     LIMIT 1",
                )
                .fetch_optional(&pool)
                .await
                .map_err(io::Error::other)?;
                decode_models_cache_row_sqlite(row, None)
            }
            RuntimeCachePool::Postgres(pool) => {
                let row = sqlx::query(
                    "SELECT provider_name, wire_api, base_url, fetched_at, etag, client_version, models_json \
                     FROM model_catalog_cache \
                     ORDER BY fetched_at DESC \
                     LIMIT 1",
                )
                .fetch_optional(&pool)
                .await
                .map_err(io::Error::other)?;
                decode_models_cache_row_postgres(row, None)
            }
        }
    }
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
    let models_json = row.get::<String, _>("models_json");
    let models = serde_json::from_str(&models_json)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;

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
    let models_json = row.get::<serde_json::Value, _>("models_json");
    let models = serde_json::from_value(models_json)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;

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
