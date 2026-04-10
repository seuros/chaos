pub mod error;
pub mod identity;
pub mod memory;
pub mod schema;
pub mod skill;

use std::path::Path;

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

pub use error::DaemonError;

/// The daemon — a portable AI identity backed by SQLite.
pub struct Daemon {
    pool: SqlitePool,
}

impl Daemon {
    /// Open (or create) a daemon identity at the given path.
    pub async fn open(path: &Path) -> Result<Self, DaemonError> {
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(Self { pool })
    }

    /// Open the default daemon for the current user.
    /// Stored at `~/.chaos/daemon.db` (or `$CHAOS_HOME/daemon.db`).
    pub async fn default() -> Result<Self, DaemonError> {
        let dir = chaos_pwd::find_chaos_home()?;
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join("daemon.db");
        Self::open(&path).await
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}
