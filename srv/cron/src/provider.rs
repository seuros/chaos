use crate::job::CreateJobParams;
use crate::job::CronJob;
use crate::job::CronScope;
use crate::store::CronStore;
use chaos_storage::ChaosStorageProvider;

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

#[cfg(test)]
mod tests {
    use super::CronStorage;
    use super::SqliteCronStorage;
    use chaos_storage::ChaosStorageProvider;

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
}
