/// Errors from the daemon's brain.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),

    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("identity not initialized — call set_identity first")]
    NoIdentity,

    #[error("memory not found: {0}")]
    MemoryNotFound(i64),
}
