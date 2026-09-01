#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("not a git repository: {0}")]
    NotARepo(String),

    #[error("git operation failed: {0}")]
    Operation(String),

    #[error("reference not found: {0}")]
    RefNotFound(String),

    #[error("path not found: {0}")]
    PathNotFound(String),

    #[error("invalid git tool input: {0}")]
    InvalidInput(String),

    #[error("path is ignored: {0}")]
    IgnoredPath(String),

    #[error("path has unresolved index conflicts: {0}")]
    Conflict(String),

    #[error("unsupported git operation: {0}")]
    Unsupported(String),

    #[error("cannot commit while repository operation is in progress: {0}")]
    RepositoryState(String),

    #[error("nothing staged to commit")]
    EmptyCommit,
}

impl From<gix::reference::find::existing::Error> for GitError {
    fn from(e: gix::reference::find::existing::Error) -> Self {
        GitError::RefNotFound(e.to_string())
    }
}
