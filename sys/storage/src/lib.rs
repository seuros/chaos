use sqlx::SqlitePool;
use std::path::Path;
use std::path::PathBuf;

const CHAOS_STORAGE_URL_ENV: &str = "CHAOS_STORAGE_URL";

/// Logical storage backend kind.
///
/// Phase 1 keeps SQLite as the only implemented adapter, but callers can
/// already reason about backend intent without leaking SQLx types everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    Sqlite,
    Postgres,
}

/// Backend bootstrap configuration resolved from config or environment.
///
/// SQLite remains the only supported adapter for now. Postgres is represented
/// explicitly so callers can move toward backend-driven configuration before
/// the Postgres adapter lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageConfig {
    SqliteHome(PathBuf),
    SqliteUrl(String),
    PostgresUrl(String),
}

impl StorageConfig {
    pub fn sqlite_home(path: impl Into<PathBuf>) -> Self {
        Self::SqliteHome(path.into())
    }

    pub fn sqlite_url(url: impl Into<String>) -> Self {
        Self::SqliteUrl(url.into())
    }

    pub fn postgres_url(url: impl Into<String>) -> Self {
        Self::PostgresUrl(url.into())
    }

    pub fn kind(&self) -> StorageKind {
        match self {
            Self::SqliteHome(_) | Self::SqliteUrl(_) => StorageKind::Sqlite,
            Self::PostgresUrl(_) => StorageKind::Postgres,
        }
    }
}

/// Backend-agnostic storage provider for ChaOS persistence.
///
/// This centralizes backend/bootstrap resolution. Domain crates should build
/// typed stores on top of it rather than resolving env vars or DB paths
/// themselves.
#[derive(Debug, Clone)]
pub struct ChaosStorageProvider {
    backend: StorageBackend,
}

#[derive(Debug, Clone)]
enum StorageBackend {
    Sqlite(SqliteStorageProvider),
}

#[derive(Debug, Clone)]
struct SqliteStorageProvider {
    pool: SqlitePool,
}

impl ChaosStorageProvider {
    /// Build a provider around an already-open SQLite pool.
    pub fn from_sqlite_pool(pool: SqlitePool) -> Self {
        Self {
            backend: StorageBackend::Sqlite(SqliteStorageProvider { pool }),
        }
    }

    /// Build a provider from an explicit storage config.
    ///
    /// SQLite is the only implemented backend in phase 1. Postgres config is
    /// accepted at the type level but rejected here until a concrete adapter is
    /// added.
    pub async fn from_config(config: StorageConfig) -> Result<Self, String> {
        match config {
            StorageConfig::SqliteHome(sqlite_home) => {
                let pool = chaos_proc::open_chaos_db(sqlite_home.as_path())
                    .await
                    .map_err(|err| format!("failed to open chaos db: {err}"))?;
                Ok(Self::from_sqlite_pool(pool))
            }
            StorageConfig::SqliteUrl(database_url) => {
                let pool = chaos_proc::open_chaos_db_url(&database_url)
                    .await
                    .map_err(|err| format!("failed to open chaos db: {err}"))?;
                Ok(Self::from_sqlite_pool(pool))
            }
            StorageConfig::PostgresUrl(_) => {
                Err("postgres storage backend is not implemented yet".to_string())
            }
        }
    }

    /// Resolve a provider from an existing pool or from the configured SQLite
    /// home.
    pub async fn from_optional_sqlite(
        existing_pool: Option<&SqlitePool>,
        sqlite_home: Option<&Path>,
    ) -> Result<Self, String> {
        if let Some(pool) = existing_pool {
            return Ok(Self::from_sqlite_pool(pool.to_owned()));
        }

        let sqlite_home = sqlite_home
            .ok_or_else(|| "chaos DB unavailable — storage provider not configured".to_string())?;
        Self::from_config(StorageConfig::sqlite_home(sqlite_home)).await
    }

    /// Resolve a provider from an existing pool or from an explicit storage
    /// config.
    pub async fn from_optional_config(
        existing_pool: Option<&SqlitePool>,
        config: Option<&StorageConfig>,
    ) -> Result<Self, String> {
        if let Some(pool) = existing_pool {
            return Ok(Self::from_sqlite_pool(pool.to_owned()));
        }

        let config = config
            .cloned()
            .ok_or_else(|| "chaos DB unavailable — storage provider not configured".to_string())?;
        Self::from_config(config).await
    }

    /// Resolve a provider from an existing pool or environment.
    ///
    /// Resolution order:
    /// 1. `CHAOS_STORAGE_URL`
    /// 2. `$CHAOS_SQLITE_HOME`
    pub async fn from_env(existing_pool: Option<&SqlitePool>) -> Result<Self, String> {
        let config = resolve_storage_config_from_env()?;
        Self::from_optional_config(existing_pool, config.as_ref()).await
    }

    pub fn kind(&self) -> StorageKind {
        match &self.backend {
            StorageBackend::Sqlite(_) => StorageKind::Sqlite,
        }
    }

    /// Transitional SQLite accessor for domain crates that still own their
    /// typed SQL implementation locally.
    pub fn sqlite_pool_cloned(&self) -> Option<SqlitePool> {
        match &self.backend {
            StorageBackend::Sqlite(provider) => Some(provider.pool.clone()),
        }
    }
}

fn resolve_storage_config_from_env() -> Result<Option<StorageConfig>, String> {
    if let Some(url) = std::env::var(CHAOS_STORAGE_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            return Ok(Some(StorageConfig::postgres_url(url)));
        }

        if url.starts_with("sqlite://") || url.starts_with("sqlite:") {
            return Ok(Some(StorageConfig::sqlite_url(url)));
        }

        if url.starts_with("sqlite3://") || url.starts_with("sqlite3:") {
            let normalized = url.replacen("sqlite3:", "sqlite:", 1);
            return Ok(Some(StorageConfig::sqlite_url(normalized)));
        }

        return Err(format!(
            "unsupported {CHAOS_STORAGE_URL_ENV} scheme; expected sqlite:, sqlite://, postgres://, or postgresql://"
        ));
    }

    Ok(resolve_sqlite_home_from_env().map(StorageConfig::sqlite_home))
}

fn resolve_sqlite_home_from_env() -> Option<PathBuf> {
    let path = PathBuf::from(chaos_proc::sqlite_home_env_value()?);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(std::env::current_dir().ok()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::CHAOS_STORAGE_URL_ENV;
    use super::ChaosStorageProvider;
    use super::StorageConfig;
    use super::StorageKind;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[tokio::test]
    async fn from_optional_sqlite_opens_shared_db() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");

        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(temp_dir.path()))
            .await
            .expect("resolve provider");

        assert!(
            provider.sqlite_pool_cloned().is_some(),
            "provider should expose sqlite pool"
        );
        assert!(
            tokio::fs::try_exists(&chaos_proc::chaos_db_path(temp_dir.path()))
                .await
                .expect("stat chaos db"),
            "expected shared chaos db file to be created"
        );
        assert_eq!(provider.kind(), StorageKind::Sqlite);
    }

    #[tokio::test]
    async fn from_config_rejects_postgres_until_adapter_exists() {
        let err = ChaosStorageProvider::from_config(StorageConfig::postgres_url(
            "postgres://ubuntu:ubuntu@localhost:5432/postgres",
        ))
        .await
        .expect_err("postgres adapter should not exist yet");

        assert!(err.contains("not implemented"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn from_env_uses_storage_url_when_present() {
        let _guard = EnvGuard::set(
            CHAOS_STORAGE_URL_ENV,
            Some("postgres://ubuntu:ubuntu@localhost:5432/postgres"),
        );

        let err = ChaosStorageProvider::from_env(None)
            .await
            .expect_err("postgres config should parse before sqlite fallback");

        assert!(err.contains("not implemented"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn from_env_accepts_sqlite_storage_url() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let db_path = chaos_proc::chaos_db_path(temp_dir.path());
        let sqlite_url = format!("sqlite://{}", db_path.display());
        let _guard = EnvGuard::set(CHAOS_STORAGE_URL_ENV, Some(&sqlite_url));

        let provider = ChaosStorageProvider::from_env(None)
            .await
            .expect("sqlite storage url should resolve");

        assert_eq!(provider.kind(), StorageKind::Sqlite);
        assert!(
            provider.sqlite_pool_cloned().is_some(),
            "sqlite provider should expose sqlite pool"
        );
        assert!(
            tokio::fs::try_exists(&db_path)
                .await
                .expect("stat chaos db"),
            "expected chaos db file to be created from sqlite url"
        );
    }

    #[test]
    fn from_env_normalizes_sqlite3_alias() {
        let _guard = EnvGuard::set(CHAOS_STORAGE_URL_ENV, Some("sqlite3:///tmp/chaos.sqlite"));

        let config = super::resolve_storage_config_from_env()
            .expect("sqlite3 alias should parse")
            .expect("storage config");

        assert_eq!(
            config,
            StorageConfig::sqlite_url("sqlite:///tmp/chaos.sqlite")
        );
    }

    #[test]
    fn from_env_rejects_unsupported_storage_url_scheme() {
        let _guard = EnvGuard::set(CHAOS_STORAGE_URL_ENV, Some("mysql://localhost/chaos"));

        let err = super::resolve_storage_config_from_env().expect_err("unsupported scheme");
        assert!(err.contains("unsupported"), "unexpected error: {err}");
    }

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
            Self {
                _lock: lock,
                key,
                previous,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
}
