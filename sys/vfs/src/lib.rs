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
mod tests;
