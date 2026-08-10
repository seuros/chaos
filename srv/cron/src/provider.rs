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
mod tests {
    use super::BackendCronStorage;
    use super::CreateJobParams;
    use super::CronScope;
    use super::CronStorage;
    use crate::Schedule;
    use chaos_vfs::ChaosVfs;
    use chaos_vfs::MountConfig;
    use chaos_vfs::Vfs;

    const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

    fn daily_schedule_json() -> String {
        Schedule::Interval { seconds: 86_400 }
            .to_json()
            .expect("serialize schedule")
    }

    fn postgres_test_url() -> Option<String> {
        std::env::var(TEST_DATABASE_URL_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    #[tokio::test]
    async fn backend_cron_storage_selects_sqlite_provider() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");

        let vfs = ChaosVfs::from_config(MountConfig::sqlite_home(temp_dir.path()))
            .await
            .expect("open sqlite home");

        let storage = BackendCronStorage::from_provider(&vfs);
        assert!(
            matches!(storage, BackendCronStorage::Sqlite(_)),
            "a sqlite mount should resolve to sqlite storage"
        );

        let jobs = storage.list(None, None).await.expect("list cron jobs");
        assert!(jobs.is_empty(), "a fresh mount should see no cron jobs");
        assert!(
            tokio::fs::try_exists(&chaos_proc::runtime_db_path(temp_dir.path()))
                .await
                .expect("stat runtime db"),
            "expected shared runtime db file to be created"
        );
    }

    #[tokio::test]
    async fn postgres_backend_cron_storage_selects_postgres_provider() {
        let Some(database_url) = postgres_test_url() else {
            eprintln!(
                "skipping postgres cron provider validation; {TEST_DATABASE_URL_ENV} is not set"
            );
            return;
        };

        let vfs = ChaosVfs::from_config(MountConfig::postgres_url(database_url))
            .await
            .expect("open postgres mount");
        assert!(
            matches!(vfs.pool(), Vfs::Postgres(_)),
            "a postgres mount should hand back a postgres pool"
        );

        let storage = BackendCronStorage::from_provider(&vfs);
        assert!(
            matches!(storage, BackendCronStorage::Postgres(_)),
            "a postgres mount should resolve to postgres storage"
        );

        // The table is shared, so the round trip runs under a path this test
        // owns rather than asserting on everything the database holds.
        let project_path = format!("/tmp/chaos-postgres/provider/{}", std::process::id());
        let job = storage
            .create(&CreateJobParams::shell(
                "postgres-provider-job".to_string(),
                daily_schedule_json(),
                "echo hi".to_string(),
                CronScope::Project,
                Some(project_path.clone()),
                None,
            ))
            .await
            .expect("create cron job through the postgres provider");

        let jobs = storage
            .list(None, Some(&project_path))
            .await
            .expect("list cron jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, job.id);

        storage.delete(&job.id).await.expect("delete cron job");
    }
}
