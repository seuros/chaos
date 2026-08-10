//! The ChaOS virtual filesystem: one interface over the SQLite and Postgres
//! backends, mounted once at boot and read through [`root`] thereafter.

pub use chaos_dispatch::backend_dispatch;

use sqlx::PgPool;
use sqlx::SqlitePool;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::RwLock;

const CHAOS_STORAGE_URL_ENV: &str = "CHAOS_STORAGE_URL";

static ROOT: RwLock<Option<&'static ChaosVfs>> = RwLock::new(None);
static MOUNTS: Mutex<Vec<(MountConfig, &'static ChaosVfs)>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, thiserror::Error)]
pub enum VfsError {
    #[error(
        "no storage backend is mounted; boot must mount one (set {CHAOS_STORAGE_URL_ENV} or a chaos home)"
    )]
    NotMounted,
    #[error("{CHAOS_STORAGE_URL_ENV}: {0}")]
    Config(String),
    #[error("failed to open runtime db: {0}")]
    Open(String),
}

/// Which backend is mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsKind {
    Sqlite,
    Postgres,
}

/// The mounted backend's live handle. Consumers that speak SQL directly match
/// on this rather than probing for one pool type at a time.
#[derive(Debug, Clone)]
pub enum Vfs {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

impl Vfs {
    pub fn kind(&self) -> VfsKind {
        match self {
            Self::Sqlite(_) => VfsKind::Sqlite,
            Self::Postgres(_) => VfsKind::Postgres,
        }
    }
}

/// What to mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountConfig {
    SqliteHome(PathBuf),
    SqliteUrl(String),
    PostgresUrl(String),
}

impl MountConfig {
    /// A chaos home's SQLite file. The path is made absolute, so two callers
    /// naming the same home from different working directories key alike.
    pub fn sqlite_home(path: impl Into<PathBuf>) -> Self {
        Self::SqliteHome(absolute(&path.into()))
    }

    pub fn sqlite_url(url: impl Into<String>) -> Self {
        Self::SqliteUrl(url.into())
    }

    pub fn postgres_url(url: impl Into<String>) -> Self {
        Self::PostgresUrl(url.into())
    }

    pub fn from_url(url: impl Into<String>) -> Result<Self, VfsError> {
        let url = url.into().trim().to_string();
        if url.is_empty() {
            return Err(VfsError::Config(
                "empty storage URL; expected sqlite:, sqlite://, postgres://, or postgresql://"
                    .to_string(),
            ));
        }

        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            return Ok(Self::postgres_url(url));
        }

        if url.starts_with("sqlite://") || url.starts_with("sqlite:") {
            return Ok(Self::sqlite_url(url));
        }

        if url.starts_with("sqlite3://") || url.starts_with("sqlite3:") {
            return Ok(Self::sqlite_url(url.replacen("sqlite3:", "sqlite:", 1)));
        }

        Err(VfsError::Config(
            "unsupported storage URL scheme; expected sqlite:, sqlite://, postgres://, or postgresql://"
                .to_string(),
        ))
    }

    pub fn kind(&self) -> VfsKind {
        match self {
            Self::SqliteHome(_) | Self::SqliteUrl(_) => VfsKind::Sqlite,
            Self::PostgresUrl(_) => VfsKind::Postgres,
        }
    }
}

/// A backend that is open and migrated, ready to serve queries.
#[derive(Debug, Clone)]
pub struct ChaosVfs {
    pool: Vfs,
}

impl ChaosVfs {
    pub fn from_sqlite_pool(pool: SqlitePool) -> Self {
        Self {
            pool: Vfs::Sqlite(pool),
        }
    }

    pub fn from_postgres_pool(pool: PgPool) -> Self {
        Self {
            pool: Vfs::Postgres(pool),
        }
    }

    /// Open the backend described by `config`, running its migrations.
    pub async fn from_config(config: MountConfig) -> Result<Self, VfsError> {
        match config {
            MountConfig::SqliteHome(sqlite_home) => chaos_proc::open_runtime_db(&sqlite_home)
                .await
                .map(Self::from_sqlite_pool)
                .map_err(|err| VfsError::Open(err.to_string())),
            MountConfig::SqliteUrl(url) => chaos_proc::open_runtime_db_url(&url)
                .await
                .map(Self::from_sqlite_pool)
                .map_err(|err| VfsError::Open(err.to_string())),
            MountConfig::PostgresUrl(url) => chaos_proc::open_runtime_db_postgres_url(&url)
                .await
                .map(Self::from_postgres_pool)
                .map_err(|err| VfsError::Open(err.to_string())),
        }
    }

    pub fn pool(&self) -> Vfs {
        self.pool.clone()
    }

    pub fn kind(&self) -> VfsKind {
        self.pool.kind()
    }

    pub fn sqlite_pool(&self) -> Option<SqlitePool> {
        match &self.pool {
            Vfs::Sqlite(pool) => Some(pool.clone()),
            Vfs::Postgres(_) => None,
        }
    }

    pub fn postgres_pool(&self) -> Option<PgPool> {
        match &self.pool {
            Vfs::Postgres(pool) => Some(pool.clone()),
            Vfs::Sqlite(_) => None,
        }
    }
}

/// Decide what to mount: an explicit storage URL, then `CHAOS_STORAGE_URL`,
/// then the chaos home's SQLite file.
pub fn resolve_mount_config(
    storage_url: Option<&str>,
    sqlite_home: &Path,
) -> Result<MountConfig, VfsError> {
    if let Some(url) = non_empty(storage_url) {
        return MountConfig::from_url(url);
    }

    if let Some(url) = std::env::var(CHAOS_STORAGE_URL_ENV)
        .ok()
        .as_deref()
        .and_then(|value| non_empty(Some(value)))
        .map(str::to_string)
    {
        return MountConfig::from_url(url);
    }

    Ok(MountConfig::sqlite_home(sqlite_home))
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// Mount `vfs` under `config` for the life of the process, or hand back the
/// backend already mounted there.
pub fn mount(config: MountConfig, vfs: ChaosVfs) -> &'static ChaosVfs {
    let mut mounts = MOUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(mounted) = lookup(&mounts, &config) {
        return mounted;
    }

    let mounted: &'static ChaosVfs = Box::leak(Box::new(vfs));
    mounts.push((config, mounted));
    mounted
}

/// Make `vfs` the root, the backend [`root`] and [`pool`] hand to consumers.
/// Boot claims it once and the process keeps it; a later nomination is dropped,
/// and the caller keeps serving itself from the handle it already holds.
pub fn set_root(vfs: &'static ChaosVfs) {
    ROOT.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert(vfs);
}

/// The backend mounted under `config`, if any.
pub fn mounted(config: &MountConfig) -> Option<&'static ChaosVfs> {
    let mounts = MOUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    lookup(&mounts, config)
}

fn lookup(
    mounts: &[(MountConfig, &'static ChaosVfs)],
    config: &MountConfig,
) -> Option<&'static ChaosVfs> {
    mounts
        .iter()
        .find(|(mounted, _)| mounted == config)
        .map(|(_, vfs)| *vfs)
}

/// The mounted backend.
pub fn root() -> Result<&'static ChaosVfs, VfsError> {
    ROOT.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ok_or(VfsError::NotMounted)
}

/// The mounted backend's pool.
pub fn pool() -> Result<Vfs, VfsError> {
    root().map(ChaosVfs::pool)
}

/// The backend serving `sqlite_home`, falling back to the root for a home this
/// process never mounted. The lookup keys on the home alone: a process serving
/// one backend from a URL mounted no home and lands on the root, while a process
/// holding several homes reaches the one it named.
pub fn root_for(sqlite_home: &Path) -> Result<&'static ChaosVfs, VfsError> {
    if let Some(vfs) = mounted(&MountConfig::sqlite_home(sqlite_home)) {
        return Ok(vfs);
    }
    root()
}

/// The pool of the backend serving `sqlite_home`.
pub fn pool_for(sqlite_home: &Path) -> Result<Vfs, VfsError> {
    root_for(sqlite_home).map(ChaosVfs::pool)
}

pub fn is_mounted() -> bool {
    root().is_ok()
}

#[cfg(test)]
mod tests {
    use super::CHAOS_STORAGE_URL_ENV;
    use super::ChaosVfs;
    use super::MountConfig;
    use super::VfsKind;
    use std::path::Path;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());
    const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

    fn postgres_test_url() -> Option<String> {
        std::env::var(TEST_DATABASE_URL_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    #[tokio::test]
    async fn lib_suite() {
        sqlite_home_opens_shared_db().await;
        from_config_reports_connection_errors_for_postgres_url().await;
        postgres_from_config_opens_postgres_runtime_schema_when_configured().await;
        sqlite_storage_url_opens_runtime_db().await;
        sqlite_in_memory_storage_url_opens_runtime_schema().await;
        mount_is_keyed_by_config_and_first_becomes_root().await;
    }

    async fn sqlite_home_opens_shared_db() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");

        let vfs = ChaosVfs::from_config(MountConfig::sqlite_home(temp_dir.path()))
            .await
            .expect("open sqlite home");

        assert_eq!(vfs.kind(), VfsKind::Sqlite);
        assert!(
            tokio::fs::try_exists(&chaos_proc::runtime_db_path(temp_dir.path()))
                .await
                .expect("stat runtime db"),
            "expected shared runtime db file to be created"
        );
    }

    async fn from_config_reports_connection_errors_for_postgres_url() {
        let err = ChaosVfs::from_config(MountConfig::postgres_url(
            "postgres://ubuntu:ubuntu@127.0.0.1:1/postgres?connect_timeout=1",
        ))
        .await
        .expect_err("postgres backend should attempt to connect");

        assert!(
            err.to_string().contains("failed to open runtime db"),
            "unexpected error: {err}"
        );
    }

    async fn postgres_from_config_opens_postgres_runtime_schema_when_configured() {
        let Some(database_url) = postgres_test_url() else {
            eprintln!("skipping postgres vfs validation; {TEST_DATABASE_URL_ENV} is not set");
            return;
        };

        let vfs = ChaosVfs::from_config(MountConfig::postgres_url(database_url))
            .await
            .expect("open postgres-backed vfs");

        assert_eq!(vfs.kind(), VfsKind::Postgres);
        assert!(
            vfs.sqlite_pool().is_none(),
            "postgres mount should not expose a sqlite pool"
        );

        let pool = vfs.postgres_pool().expect("postgres pool");
        let cron_jobs_table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('public.cron_jobs')::text")
                .fetch_one(&pool)
                .await
                .expect("query postgres runtime schema");
        assert_eq!(cron_jobs_table.as_deref(), Some("cron_jobs"));
    }

    async fn sqlite_storage_url_opens_runtime_db() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let db_path = chaos_proc::runtime_db_path(temp_dir.path());
        let sqlite_url = format!("sqlite://{}", db_path.display());

        let config = {
            let _guard = EnvGuard::set(CHAOS_STORAGE_URL_ENV, Some(&sqlite_url));
            super::resolve_mount_config(None, temp_dir.path()).expect("resolve from env")
        };
        assert_eq!(config, MountConfig::sqlite_url(&sqlite_url));

        let vfs = ChaosVfs::from_config(config)
            .await
            .expect("open sqlite url");
        assert_eq!(vfs.kind(), VfsKind::Sqlite);
        assert!(
            tokio::fs::try_exists(&db_path)
                .await
                .expect("stat runtime db"),
            "expected runtime db file to be created from sqlite url"
        );
    }

    async fn sqlite_in_memory_storage_url_opens_runtime_schema() {
        let vfs = ChaosVfs::from_config(MountConfig::sqlite_url("sqlite::memory:"))
            .await
            .expect("open in-memory sqlite");

        let pool = vfs.sqlite_pool().expect("sqlite pool");
        let table_exists: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE name = 'processes'")
                .fetch_optional(&pool)
                .await
                .expect("query in-memory sqlite schema");
        assert_eq!(table_exists.as_deref(), Some("processes"));
    }

    async fn mount_is_keyed_by_config_and_first_becomes_root() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let config = MountConfig::sqlite_home(temp_dir.path());
        let vfs = ChaosVfs::from_config(config.clone())
            .await
            .expect("open sqlite home");

        let unmounted = super::root().expect_err("nothing should be mounted yet");
        assert!(
            unmounted.to_string().contains(CHAOS_STORAGE_URL_ENV),
            "an unmounted backend should point at {CHAOS_STORAGE_URL_ENV}, got {unmounted}"
        );
        assert!(super::mounted(&config).is_none());

        let mounted = super::mount(config.clone(), vfs.clone());
        super::set_root(mounted);
        assert!(super::is_mounted());
        assert_eq!(super::root().expect("root").kind(), VfsKind::Sqlite);
        assert!(std::ptr::eq(
            mounted,
            super::mounted(&config).expect("mounted under its config")
        ));
        assert!(
            std::ptr::eq(mounted, super::mount(config, vfs)),
            "re-mounting a config should hand back the same backend"
        );

        let other = tempfile::tempdir().expect("create temp dir");
        let other_config = MountConfig::sqlite_home(other.path());
        let other_vfs = ChaosVfs::from_config(other_config.clone())
            .await
            .expect("open sqlite home");
        let other_mounted = super::mount(other_config, other_vfs);
        assert!(
            !std::ptr::eq(mounted, other_mounted),
            "a different config should get its own backend"
        );
        super::set_root(other_mounted);
        assert!(
            std::ptr::eq(mounted, super::root().expect("root")),
            "root should stay the first mount"
        );

        let _guard = EnvGuard::set(
            CHAOS_STORAGE_URL_ENV,
            Some("postgres://ubuntu:ubuntu@localhost:5432/postgres"),
        );
        assert!(
            std::ptr::eq(
                other_mounted,
                super::root_for(other.path()).expect("the home it named")
            ),
            "a mounted home should serve the caller that names it"
        );
        let never_mounted = tempfile::tempdir().expect("create temp dir");
        assert!(
            std::ptr::eq(
                mounted,
                super::root_for(never_mounted.path()).expect("root")
            ),
            "a home this process never mounted should land on the root"
        );
    }

    #[test]
    fn mount_config_from_url_parses_supported_schemes() {
        assert_eq!(
            MountConfig::from_url("postgresql://ubuntu:ubuntu@localhost/chaos")
                .expect("postgresql URL"),
            MountConfig::postgres_url("postgresql://ubuntu:ubuntu@localhost/chaos")
        );
        assert_eq!(
            MountConfig::from_url(" sqlite3:///tmp/chaos.sqlite ").expect("sqlite3 URL"),
            MountConfig::sqlite_url("sqlite:///tmp/chaos.sqlite")
        );
        assert!(MountConfig::from_url("mysql://localhost/chaos").is_err());
        assert!(MountConfig::from_url("   ").is_err());
    }

    #[test]
    fn resolve_mount_config_prefers_explicit_url_then_env_then_home() {
        let guard = EnvGuard::set(
            CHAOS_STORAGE_URL_ENV,
            Some("postgres://ubuntu:ubuntu@localhost:5432/postgres"),
        );

        assert_eq!(
            super::resolve_mount_config(Some("sqlite:///tmp/explicit.sqlite"), Path::new("/tmp"))
                .expect("explicit url wins"),
            MountConfig::sqlite_url("sqlite:///tmp/explicit.sqlite")
        );
        assert_eq!(
            super::resolve_mount_config(None, Path::new("/tmp")).expect("env url"),
            MountConfig::postgres_url("postgres://ubuntu:ubuntu@localhost:5432/postgres")
        );

        drop(guard);
        let _guard = EnvGuard::set(CHAOS_STORAGE_URL_ENV, None);
        assert_eq!(
            super::resolve_mount_config(None, Path::new("/tmp/chaos-home")).expect("home fallback"),
            MountConfig::sqlite_home("/tmp/chaos-home")
        );
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
