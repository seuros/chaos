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
