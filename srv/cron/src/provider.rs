use crate::job::CreateJobParams;
use crate::job::CronJob;
use crate::job::CronScope;
use crate::store::CronStore;
use crate::store::PostgresCronStore;
use chaos_storage::ChaosStorageProvider;
use chaos_storage::StorageKind;

/// Native async trait for cron persistence operations.
pub(crate) trait CronStorage: Send + Sync {
    async fn create(&self, params: &CreateJobParams) -> anyhow::Result<CronJob>;
    async fn list(
        &self,
        scope: Option<CronScope>,
        project_path: Option<&str>,
    ) -> anyhow::Result<Vec<CronJob>>;
    async fn get(&self, id: &str) -> anyhow::Result<Option<CronJob>>;
    async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()>;
    async fn delete(&self, id: &str) -> anyhow::Result<()>;
}

/// Concrete SQLite-backed cron storage built on top of the shared ChaOS
/// storage provider.
#[derive(Clone)]
pub(crate) struct SqliteCronStorage {
    store: CronStore,
}

impl SqliteCronStorage {
    pub fn from_provider(provider: &ChaosStorageProvider) -> Result<Self, String> {
        let pool = provider.sqlite_pool_cloned().ok_or_else(|| {
            "chaos DB unavailable — cron storage backend not supported".to_string()
        })?;
        Ok(Self {
            store: CronStore::new(pool),
        })
    }
}

impl CronStorage for SqliteCronStorage {
    async fn create(&self, params: &CreateJobParams) -> anyhow::Result<CronJob> {
        self.store.create(params).await
    }

    async fn list(
        &self,
        scope: Option<CronScope>,
        project_path: Option<&str>,
    ) -> anyhow::Result<Vec<CronJob>> {
        self.store.list(scope, project_path).await
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<CronJob>> {
        self.store.get(id).await
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        self.store.set_enabled(id, enabled).await
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete(id).await
    }
}

/// Concrete Postgres-backed cron storage built on top of the shared ChaOS
/// storage provider.
#[derive(Clone)]
pub(crate) struct PostgresCronStorage {
    store: PostgresCronStore,
}

impl PostgresCronStorage {
    pub fn from_provider(provider: &ChaosStorageProvider) -> Result<Self, String> {
        let pool = provider.postgres_pool_cloned().ok_or_else(|| {
            "chaos DB unavailable — cron storage backend not supported".to_string()
        })?;
        Ok(Self {
            store: PostgresCronStore::new(pool),
        })
    }
}

impl CronStorage for PostgresCronStorage {
    async fn create(&self, params: &CreateJobParams) -> anyhow::Result<CronJob> {
        self.store.create(params).await
    }

    async fn list(
        &self,
        scope: Option<CronScope>,
        project_path: Option<&str>,
    ) -> anyhow::Result<Vec<CronJob>> {
        self.store.list(scope, project_path).await
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<CronJob>> {
        self.store.get(id).await
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        self.store.set_enabled(id, enabled).await
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete(id).await
    }
}

#[derive(Clone)]
pub(crate) enum BackendCronStorage {
    Postgres(PostgresCronStorage),
    Sqlite(SqliteCronStorage),
}

impl BackendCronStorage {
    pub fn from_provider(provider: &ChaosStorageProvider) -> Result<Self, String> {
        match provider.kind() {
            StorageKind::Sqlite => SqliteCronStorage::from_provider(provider).map(Self::Sqlite),
            StorageKind::Postgres => {
                PostgresCronStorage::from_provider(provider).map(Self::Postgres)
            }
        }
    }

    pub async fn due_now(&self) -> anyhow::Result<Vec<CronJob>> {
        match self {
            Self::Sqlite(storage) => storage.store.due_now().await,
            Self::Postgres(storage) => storage.store.due_now().await,
        }
    }

    pub async fn mark_run(&self, id: &str, next_run_at: Option<i64>) -> anyhow::Result<()> {
        match self {
            Self::Sqlite(storage) => storage.store.mark_run(id, next_run_at).await,
            Self::Postgres(storage) => storage.store.mark_run(id, next_run_at).await,
        }
    }
}

impl CronStorage for BackendCronStorage {
    async fn create(&self, params: &CreateJobParams) -> anyhow::Result<CronJob> {
        match self {
            Self::Sqlite(storage) => storage.create(params).await,
            Self::Postgres(storage) => storage.create(params).await,
        }
    }

    async fn list(
        &self,
        scope: Option<CronScope>,
        project_path: Option<&str>,
    ) -> anyhow::Result<Vec<CronJob>> {
        match self {
            Self::Sqlite(storage) => storage.list(scope, project_path).await,
            Self::Postgres(storage) => storage.list(scope, project_path).await,
        }
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<CronJob>> {
        match self {
            Self::Sqlite(storage) => storage.get(id).await,
            Self::Postgres(storage) => storage.get(id).await,
        }
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        match self {
            Self::Sqlite(storage) => storage.set_enabled(id, enabled).await,
            Self::Postgres(storage) => storage.set_enabled(id, enabled).await,
        }
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        match self {
            Self::Sqlite(storage) => storage.delete(id).await,
            Self::Postgres(storage) => storage.delete(id).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BackendCronStorage;
    use super::CronStorage;
    use super::PostgresCronStorage;
    use super::SqliteCronStorage;
    use chaos_storage::ChaosStorageProvider;
    use chaos_storage::StorageConfig;

    const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

    fn postgres_test_url() -> Option<String> {
        std::env::var(TEST_DATABASE_URL_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    #[tokio::test]
    async fn cron_storage_wraps_shared_provider() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");

        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(temp_dir.path()))
            .await
            .expect("resolve provider");

        let jobs = SqliteCronStorage::from_provider(&provider)
            .expect("cron storage")
            .list(None, None)
            .await
            .expect("list cron jobs");
        assert!(jobs.is_empty(), "fresh provider should see no cron jobs");
        assert!(
            tokio::fs::try_exists(&chaos_proc::runtime_db_path(temp_dir.path()))
                .await
                .expect("stat runtime db"),
            "expected shared runtime db file to be created"
        );
    }

    #[tokio::test]
    async fn backend_cron_storage_selects_sqlite_provider() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");

        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(temp_dir.path()))
            .await
            .expect("resolve provider");

        let storage = BackendCronStorage::from_provider(&provider).expect("backend cron storage");
        assert!(
            matches!(storage, BackendCronStorage::Sqlite(_)),
            "sqlite provider should resolve to sqlite storage"
        );

        let jobs = storage.list(None, None).await.expect("list cron jobs");
        assert!(jobs.is_empty(), "fresh provider should see no cron jobs");
    }

    #[tokio::test]
    async fn postgres_backend_cron_storage_selects_postgres_provider() {
        let Some(database_url) = postgres_test_url() else {
            eprintln!(
                "skipping postgres cron provider validation; {TEST_DATABASE_URL_ENV} is not set"
            );
            return;
        };

        let provider = ChaosStorageProvider::from_config(StorageConfig::postgres_url(database_url))
            .await
            .expect("resolve postgres provider");

        let storage = BackendCronStorage::from_provider(&provider).expect("backend cron storage");
        assert!(
            matches!(storage, BackendCronStorage::Postgres(_)),
            "postgres provider should resolve to postgres storage"
        );

        let jobs = storage.list(None, None).await.expect("list cron jobs");
        assert!(jobs.is_empty(), "fresh provider should see no cron jobs");

        let direct_jobs = PostgresCronStorage::from_provider(&provider)
            .expect("postgres cron storage")
            .list(None, None)
            .await
            .expect("list cron jobs");
        assert!(
            direct_jobs.is_empty(),
            "fresh postgres provider should see no cron jobs"
        );
    }
}
