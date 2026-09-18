use crate::job::CreateJobParams;
use crate::job::CronJob;
use crate::job::CronScope;
use crate::store::CronStore;
use crate::store::PostgresCronStore;
use chaos_vfs::ChaosVfs;
use chaos_vfs::Vfs;

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

impl CronStorage for CronStore {
    async fn create(&self, params: &CreateJobParams) -> anyhow::Result<CronJob> {
        self.create(params).await
    }

    async fn list(
        &self,
        scope: Option<CronScope>,
        project_path: Option<&str>,
    ) -> anyhow::Result<Vec<CronJob>> {
        self.list(scope, project_path).await
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<CronJob>> {
        self.get(id).await
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        self.set_enabled(id, enabled).await
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.delete(id).await
    }
}

impl CronStorage for PostgresCronStore {
    async fn create(&self, params: &CreateJobParams) -> anyhow::Result<CronJob> {
        self.create(params).await
    }

    async fn list(
        &self,
        scope: Option<CronScope>,
        project_path: Option<&str>,
    ) -> anyhow::Result<Vec<CronJob>> {
        self.list(scope, project_path).await
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<CronJob>> {
        self.get(id).await
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        self.set_enabled(id, enabled).await
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.delete(id).await
    }
}

/// Cron persistence on whichever filesystem is mounted.
#[derive(Clone)]
pub(crate) enum BackendCronStorage {
    Postgres(PostgresCronStore),
    Sqlite(CronStore),
}

impl BackendCronStorage {
    pub fn from_provider(vfs: &ChaosVfs) -> Self {
        match vfs.pool() {
            Vfs::Sqlite(pool) => Self::Sqlite(CronStore::new(pool)),
            Vfs::Postgres(pool) => Self::Postgres(PostgresCronStore::new(pool)),
        }
    }

    chaos_vfs::backend_dispatch! {
        pub async fn due_now(&self) -> anyhow::Result<Vec<CronJob>>;
        pub async fn mark_run(&self, id: &str, next_run_at: Option<i64>) -> anyhow::Result<()>;
        pub async fn delete_spool_jobs_for_manifest_except(
            &self,
            manifest_id: &str,
            keep_id: Option<&str>,
        ) -> anyhow::Result<u64>;
    }
}

impl CronStorage for BackendCronStorage {
    chaos_vfs::backend_dispatch! {
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
}

#[cfg(test)]
mod tests;
