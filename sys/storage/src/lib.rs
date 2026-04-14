use sqlx::PgPool;
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
/// SQLite and Postgres are both represented explicitly so callers can move
/// toward backend-driven configuration without hard-coding one engine.
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
    Postgres(PostgresStorageProvider),
    Sqlite(SqliteStorageProvider),
}

#[derive(Debug, Clone)]
struct SqliteStorageProvider {
    pool: SqlitePool,
}

#[derive(Debug, Clone)]
struct PostgresStorageProvider {
    pool: PgPool,
}

impl ChaosStorageProvider {
    /// Build a provider around an already-open SQLite pool.
    pub fn from_sqlite_pool(pool: SqlitePool) -> Self {
        Self {
            backend: StorageBackend::Sqlite(SqliteStorageProvider { pool }),
        }
    }

    /// Build a provider around an already-open Postgres pool.
    pub fn from_postgres_pool(pool: PgPool) -> Self {
        Self {
            backend: StorageBackend::Postgres(PostgresStorageProvider { pool }),
        }
    }

    /// Build a provider from an explicit storage config.
    ///
    /// SQLite and Postgres are both supported here; higher-level consumers may
    /// still choose to only expose a subset of backends.
    pub async fn from_config(config: StorageConfig) -> Result<Self, String> {
        match config {
            StorageConfig::SqliteHome(sqlite_home) => {
                let pool = chaos_proc::open_runtime_db(sqlite_home.as_path())
                    .await
                    .map_err(|err| format!("failed to open runtime db: {err}"))?;
                Ok(Self::from_sqlite_pool(pool))
            }
            StorageConfig::SqliteUrl(database_url) => {
                let pool = chaos_proc::open_runtime_db_url(&database_url)
                    .await
                    .map_err(|err| format!("failed to open runtime db: {err}"))?;
                Ok(Self::from_sqlite_pool(pool))
            }
            StorageConfig::PostgresUrl(database_url) => {
                let pool = chaos_proc::open_runtime_db_postgres_url(&database_url)
                    .await
                    .map_err(|err| format!("failed to open runtime db: {err}"))?;
                Ok(Self::from_postgres_pool(pool))
            }
        }
    }

    /// Resolve a provider from an existing SQLite pool or from the configured
    /// SQLite home.
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

    /// Resolve a provider from an existing SQLite pool or from an explicit storage
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
            StorageBackend::Postgres(_) => StorageKind::Postgres,
            StorageBackend::Sqlite(_) => StorageKind::Sqlite,
        }
    }

    /// Transitional SQLite accessor for domain crates that still own their
    /// typed SQL implementation locally.
    pub fn sqlite_pool_cloned(&self) -> Option<SqlitePool> {
        match &self.backend {
            StorageBackend::Postgres(_) => None,
            StorageBackend::Sqlite(provider) => Some(provider.pool.clone()),
        }
    }

    /// Transitional Postgres accessor for consumers that support the Postgres
    /// backend explicitly.
    pub fn postgres_pool_cloned(&self) -> Option<PgPool> {
        match &self.backend {
            StorageBackend::Postgres(provider) => Some(provider.pool.clone()),
            StorageBackend::Sqlite(_) => None,
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
    const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

    fn postgres_test_url() -> Option<String> {
        std::env::var(TEST_DATABASE_URL_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn unreachable_postgres_url() -> &'static str {
        "postgres://ubuntu:ubuntu@127.0.0.1:1/postgres?connect_timeout=1"
    }

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
            tokio::fs::try_exists(&chaos_proc::runtime_db_path(temp_dir.path()))
                .await
                .expect("stat runtime db"),
            "expected shared runtime db file to be created"
        );
        assert_eq!(provider.kind(), StorageKind::Sqlite);
    }

    #[tokio::test]
    async fn from_config_reports_connection_errors_for_postgres_url() {
        let err = ChaosStorageProvider::from_config(StorageConfig::postgres_url(
            unreachable_postgres_url(),
        ))
        .await
        .expect_err("postgres adapter should attempt to connect");

        assert!(
            err.contains("failed to open runtime db"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn postgres_from_config_opens_postgres_runtime_schema_when_configured() {
        let Some(database_url) = postgres_test_url() else {
            eprintln!("skipping postgres storage validation; {TEST_DATABASE_URL_ENV} is not set");
            return;
        };

        let provider = ChaosStorageProvider::from_config(StorageConfig::postgres_url(database_url))
            .await
            .expect("open postgres-backed storage provider");

        assert_eq!(provider.kind(), StorageKind::Postgres);
        assert!(
            provider.sqlite_pool_cloned().is_none(),
            "postgres provider should not expose a sqlite pool"
        );

        let pool = provider
            .postgres_pool_cloned()
            .expect("provider should expose postgres pool");
        let cron_jobs_table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('public.cron_jobs')::text")
                .fetch_one(&pool)
                .await
                .expect("query postgres runtime schema");
        assert_eq!(cron_jobs_table.as_deref(), Some("cron_jobs"));
    }

    #[tokio::test]
    async fn from_env_prefers_postgres_storage_url_when_present() {
        let _guard = EnvGuard::set(
            CHAOS_STORAGE_URL_ENV,
            Some("postgres://ubuntu:ubuntu@localhost:5432/postgres"),
        );

        let config = super::resolve_storage_config_from_env()
            .expect("postgres storage url should parse")
            .expect("storage config");

        assert_eq!(
            config,
            StorageConfig::postgres_url("postgres://ubuntu:ubuntu@localhost:5432/postgres")
        );
    }

    #[tokio::test]
    async fn postgres_from_env_opens_postgres_runtime_schema_when_configured() {
        let Some(database_url) = postgres_test_url() else {
            eprintln!("skipping postgres storage validation; {TEST_DATABASE_URL_ENV} is not set");
            return;
        };
        let _guard = EnvGuard::set(CHAOS_STORAGE_URL_ENV, Some(&database_url));

        let provider = ChaosStorageProvider::from_env(None)
            .await
            .expect("resolve postgres provider from env");

        assert_eq!(provider.kind(), StorageKind::Postgres);
        assert!(
            provider.postgres_pool_cloned().is_some(),
            "postgres provider should expose postgres pool"
        );
    }

    #[tokio::test]
    async fn from_env_accepts_sqlite_storage_url() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let db_path = chaos_proc::runtime_db_path(temp_dir.path());
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
                .expect("stat runtime db"),
            "expected runtime db file to be created from sqlite url"
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
