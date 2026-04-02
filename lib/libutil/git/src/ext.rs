use crate::error::GitError;

/// Extension trait for converting any `Result<T, E>` where `E: Display`
/// into `Result<T, GitError::Operation>` with minimal boilerplate.
pub(crate) trait GitResultExt<T> {
    fn git_op(self) -> Result<T, GitError>;
}

impl<T, E: std::fmt::Display> GitResultExt<T> for Result<T, E> {
    fn git_op(self) -> Result<T, GitError> {
        self.map_err(|e| GitError::Operation(e.to_string()))
    }
}
