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
}

impl From<gix::reference::find::existing::Error> for GitError {
    fn from(e: gix::reference::find::existing::Error) -> Self {
        GitError::RefNotFound(e.to_string())
    }
}
