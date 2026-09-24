//! `chaos-git` — pure-Rust git inspection for the kernel and TUI.
//!
//! Provides structured access to repository state without shelling out to
//! `git(1)`. Built on `gix` (gitoxide).
//!
//! Model-facing git tools live in the `skipper` driver.

mod branches;
mod diff;
mod error;
mod ext;
mod log;
mod remotes;
mod repo;
mod status;

pub use branches::BranchInfo;
pub use diff::DiffFile;
pub use diff::DiffFormat;
pub use diff::DiffReport;
pub use diff::DiffScope;
pub use diff::DiffStatus;
pub use diff::DiffSummary;
pub use diff::WhitespaceError;
pub use diff::diff;
pub use diff::diff_report;
pub use error::GitError;
pub use log::LogEntry;
pub use log::log;
pub use remotes::RemoteInfo;
pub use repo::RepoInfo;
pub use status::FileStatus;
pub use status::StatusInfo;

use std::path::Path;

/// Open a repository from a working directory path.
/// Walks up to find `.git`.
fn open_repo(cwd: &Path) -> Result<gix::Repository, GitError> {
    gix::discover(cwd).map_err(|e| GitError::NotARepo(e.to_string()))
}

/// Snapshot of repository identity and state.
pub fn repo_info(cwd: &Path) -> Result<RepoInfo, GitError> {
    repo::info(cwd)
}

/// Discover the worktree root (or bare git directory) without inspecting status.
pub fn repo_root(cwd: &Path) -> Result<std::path::PathBuf, GitError> {
    let repo = open_repo(cwd)?;
    Ok(repo
        .workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf())
}

/// Staged, unstaged, untracked files.
pub fn status(cwd: &Path) -> Result<StatusInfo, GitError> {
    status::collect(cwd)
}

/// Current, default, local, remote branches.
pub fn branches(cwd: &Path) -> Result<BranchInfo, GitError> {
    branches::collect(cwd)
}

/// Remote name→url map.
pub fn remotes(cwd: &Path) -> Result<RemoteInfo, GitError> {
    remotes::collect(cwd)
}

#[cfg(test)]
mod tests;
