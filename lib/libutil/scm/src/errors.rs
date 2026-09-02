use std::path::PathBuf;
use std::process::ExitStatus;
use std::string::FromUtf8Error;

use thiserror::Error;

/// Errors returned by git tooling helpers.
#[derive(Debug, Error)]
pub enum GitToolingError {
    #[error("git command `{command}` failed with status {status}: {stderr}")]
    GitCommand {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
    #[error("git command `{command}` produced non-UTF-8 output")]
    GitOutputUtf8 {
        command: String,
        #[source]
        source: FromUtf8Error,
    },
    #[error("{path:?} is not a git repository")]
    NotAGitRepository { path: PathBuf },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
