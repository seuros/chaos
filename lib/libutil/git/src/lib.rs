//! `chaos-git` — pure-Rust read-only git introspection.
//!
//! Provides structured access to repository state without shelling out to
//! `git(1)`. Built on `gix` (gitoxide). All operations are read-only.
//!
//! ## MCP surface
//!
//! **Resources** (no params, implicit cwd):
//! - `git://repo` — root, branch, HEAD sha, remote url, dirty flag, default branch
//! - `git://status` — staged, unstaged, untracked files
//! - `git://branches` — current, default, local, remote
//! - `git://remotes` — name→url map
//! - `git://diff_to_remote` — base sha + diff against closest remote ancestor
//!
//! **Tools** (require params):
//! - `diff` — unified diff with optional base ref and path filters
//! - `log` — commit history with optional limit and branch
//! - `blame` — per-line attribution for a file

mod blame;
mod branches;
mod diff;
mod error;
mod ext;
mod log;
mod remotes;
mod repo;
mod status;

pub use blame::BlameLine;
pub use blame::blame;
pub use branches::BranchInfo;
pub use diff::diff;
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

// ── Resources (no params) ──────────────────────────────────────────

/// `git://repo` — snapshot of repository identity and state.
pub fn repo_info(cwd: &Path) -> Result<RepoInfo, GitError> {
    repo::info(cwd)
}

/// `git://status` — staged, unstaged, untracked files.
pub fn status(cwd: &Path) -> Result<StatusInfo, GitError> {
    status::collect(cwd)
}

/// `git://branches` — current, default, local, remote branches.
pub fn branches(cwd: &Path) -> Result<BranchInfo, GitError> {
    branches::collect(cwd)
}

/// `git://remotes` — remote name→url map.
pub fn remotes(cwd: &Path) -> Result<RemoteInfo, GitError> {
    remotes::collect(cwd)
}
