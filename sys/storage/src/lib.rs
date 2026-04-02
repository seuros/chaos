use sqlx::SqlitePool;
use std::path::Path;
use std::path::PathBuf;

/// Backend-agnostic storage provider for ChaOS persistence.
///
/// This centralizes backend/bootstrap resolution. Domain crates should build
/// typed stores on top of it rather than resolving env vars or DB paths
/// themselves.
#[derive(Clone)]
pub struct ChaosStorageProvider {
    backend: StorageBackend,
}

#[derive(Clone)]
enum StorageBackend {
    Sqlite(SqliteStorageProvider),
}

#[derive(Clone)]
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
        let pool = chaos_proc::open_chaos_db(sqlite_home)
            .await
            .map_err(|err| format!("failed to open chaos db: {err}"))?;
        Ok(Self::from_sqlite_pool(pool))
    }

    /// Resolve a provider from an existing pool or `$CHAOS_SQLITE_HOME`.
    pub async fn from_env(existing_pool: Option<&SqlitePool>) -> Result<Self, String> {
        let sqlite_home = resolve_sqlite_home_from_env();
        Self::from_optional_sqlite(existing_pool, sqlite_home.as_deref()).await
    }

    /// Transitional SQLite accessor for domain crates that still own their
    /// typed SQL implementation locally.
    pub fn sqlite_pool_cloned(&self) -> Option<SqlitePool> {
        match &self.backend {
            StorageBackend::Sqlite(provider) => Some(provider.pool.clone()),
        }
    }
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
    use super::ChaosStorageProvider;

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
    }
}
