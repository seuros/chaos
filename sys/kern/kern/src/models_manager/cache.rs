use chaos_ipc::openai_models::ModelInfo;
use chaos_proc::open_runtime_db;
use chaos_proc::runtime_db_path;
use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;
use sqlx::Row;
use sqlx::SqlitePool;
use std::io;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::OnceCell;
use tracing::error;
use tracing::info;

/// Manages loading and saving of model catalogs in the shared runtime SQLite DB.
#[derive(Debug)]
pub(crate) struct ModelsCacheManager {
    sqlite_home: PathBuf,
    cache_ttl: Duration,
    chaos_pool: OnceCell<Option<SqlitePool>>,
}

impl ModelsCacheManager {
    /// Create a new cache manager backed by the shared runtime SQLite database.
    pub(crate) fn new(sqlite_home: PathBuf, cache_ttl: Duration) -> Self {
        Self {
            sqlite_home,
            cache_ttl,
            chaos_pool: OnceCell::new(),
        }
    }

    /// Attempt to load a fresh cache entry. Returns `None` if the cache doesn't exist, is stale,
    /// or was written for a different provider scope.
    pub(crate) async fn load_fresh(
        &self,
        expected_version: &str,
        expected_scope: &ModelsCacheScope,
    ) -> Option<ModelsCache> {
        let cache_db_path = runtime_db_path(&self.sqlite_home);
        info!(
                cache_db_path = %cache_db_path.display(),
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
            cache_db_path = %cache_db_path.display(),
            cached_version = ?cache.client_version,
            fetched_at = %cache.fetched_at,
            "models cache: loaded cache row"
        );
        if cache.client_version.as_deref() != Some(expected_version) {
            info!(
                cache_db_path = %cache_db_path.display(),
                expected_version,
                cached_version = ?cache.client_version,
                "models cache: cache version mismatch"
            );
            return None;
        }
        if !cache.is_fresh(self.cache_ttl) {
            info!(
                cache_db_path = %cache_db_path.display(),
                cache_ttl_secs = self.cache_ttl.as_secs(),
                fetched_at = %cache.fetched_at,
                "models cache: cache is stale"
            );
            return None;
        }
        info!(
            cache_db_path = %cache_db_path.display(),
            cache_ttl_secs = self.cache_ttl.as_secs(),
            "models cache: cache hit"
        );
        Some(cache)
    }

    /// Persist the cache to disk, creating parent directories as needed.
    pub(crate) async fn persist_cache(
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
    pub(crate) async fn renew_cache_ttl(
        &self,
        expected_scope: &ModelsCacheScope,
    ) -> io::Result<()> {
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

        let Some(row) = row else {
            return Ok(None);
        };

        let fetched_at = row.get::<i64, _>("fetched_at");
        let fetched_at = Timestamp::from_second(fetched_at).map_err(io::Error::other)?;
        let models_json = row.get::<String, _>("models_json");
        let models = serde_json::from_str(&models_json)
            .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;

        Ok(Some(ModelsCache {
            fetched_at,
            etag: row.get::<Option<String>, _>("etag"),
            client_version: row.get::<Option<String>, _>("client_version"),
            scope: Some(scope.clone()),
            models,
        }))
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

    async fn runtime_pool(&self) -> Option<SqlitePool> {
        self.chaos_pool
            .get_or_init(|| async {
                match open_runtime_db(&self.sqlite_home).await {
                    Ok(pool) => Some(pool),
                    Err(err) => {
                        error!(
                            "failed to open runtime db for model cache at {}: {err}",
                            runtime_db_path(&self.sqlite_home).display()
                        );
                        None
                    }
                }
            })
            .await
            .clone()
    }

    #[cfg(test)]
    /// Set the cache TTL.
    pub(crate) fn set_ttl(&mut self, ttl: Duration) {
        self.cache_ttl = ttl;
    }

    #[cfg(test)]
    /// Manipulate the newest cached catalog for testing. Allows setting a custom fetched_at timestamp.
    pub(crate) async fn manipulate_cache_for_test<F>(&self, f: F) -> io::Result<()>
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

    #[cfg(test)]
    /// Mutate the newest cached catalog for testing.
    pub(crate) async fn mutate_cache_for_test<F>(&self, f: F) -> io::Result<()>
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

    #[cfg(test)]
    async fn load_first_for_test(&self) -> io::Result<Option<ModelsCache>> {
        let Some(pool) = self.runtime_pool().await else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT provider_name, wire_api, base_url, fetched_at, etag, client_version, models_json \
             FROM model_catalog_cache \
             ORDER BY fetched_at DESC \
             LIMIT 1",
        )
        .fetch_optional(&pool)
        .await
        .map_err(io::Error::other)?;
        let Some(row) = row else {
            return Ok(None);
        };

        let scope = ModelsCacheScope {
            provider_name: row.get::<String, _>("provider_name"),
            wire_api: row.get::<String, _>("wire_api"),
            base_url: row.get::<String, _>("base_url"),
        };
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
}

/// Serialized snapshot of models and metadata cached on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModelsCache {
    pub(crate) fetched_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<ModelsCacheScope>,
    pub(crate) models: Vec<ModelInfo>,
}

/// Provider identity for a cached model catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ModelsCacheScope {
    pub(crate) provider_name: String,
    pub(crate) wire_api: String,
    pub(crate) base_url: String,
}

impl ModelsCache {
    /// Returns `true` when the cache entry has not exceeded the configured TTL.
    fn is_fresh(&self, ttl: Duration) -> bool {
        if ttl.is_zero() {
            return false;
        }
        let age_secs = Timestamp::now().as_second() - self.fetched_at.as_second();
        age_secs >= 0 && (age_secs as u64) <= ttl.as_secs()
    }
}
