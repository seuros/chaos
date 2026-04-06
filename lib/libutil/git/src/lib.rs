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
//! - `show` — full commit details with subject, body, and trailers
//! - `blame` — per-line attribution for a file

mod blame;
mod branches;
mod diff;
mod error;
mod ext;
mod log;
mod remotes;
mod repo;
mod show;
mod status;
mod tools;

pub use blame::BlameLine;
pub use blame::blame;
pub use branches::BranchInfo;
use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::CatalogToolDriver;
use chaos_traits::catalog::CatalogToolDriverFuture;
use chaos_traits::catalog::CatalogToolRequest;
use chaos_traits::catalog::CatalogToolResult;
use chaos_traits::catalog::tool_infos_to_catalog_tools;
pub use diff::diff;
pub use error::GitError;
pub use log::LogEntry;
pub use log::log;
use mcp_host::prelude::*;
pub use remotes::RemoteInfo;
pub use repo::RepoInfo;
pub use show::CommitTrailer;
pub use show::ShowEntry;
pub use show::show;
pub use status::FileStatus;
pub use status::StatusInfo;
use std::sync::Arc;

use std::path::Path;

pub struct GitServer;
pub type GitCtx<'a> = Ctx<'a>;

struct GitToolDriver;

impl CatalogToolDriver for GitToolDriver {
    fn call_tool(&self, request: CatalogToolRequest) -> CatalogToolDriverFuture<'_> {
        Box::pin(async move {
            let result = match request.tool_name.as_str() {
                "git_diff" => serde_json::from_value(request.arguments)
                    .map_err(|e| format!("invalid arguments: {e}"))
                    .and_then(|params| tools::execute_git_diff(&request.cwd, params)),
                "git_log" => serde_json::from_value(request.arguments)
                    .map_err(|e| format!("invalid arguments: {e}"))
                    .and_then(|params| tools::execute_git_log(&request.cwd, params)),
                "git_show" => serde_json::from_value(request.arguments)
                    .map_err(|e| format!("invalid arguments: {e}"))
                    .and_then(|params| tools::execute_git_show(&request.cwd, params)),
                "git_blame" => serde_json::from_value(request.arguments)
                    .map_err(|e| format!("invalid arguments: {e}"))
                    .and_then(|params| tools::execute_git_blame(&request.cwd, params)),
                "git_repo" => serde_json::from_value(request.arguments)
                    .map_err(|e| format!("invalid arguments: {e}"))
                    .and_then(|params| tools::execute_git_repo(&request.cwd, params)),
                "git_status" => serde_json::from_value(request.arguments)
                    .map_err(|e| format!("invalid arguments: {e}"))
                    .and_then(|params| tools::execute_git_status(&request.cwd, params)),
                "git_branches" => serde_json::from_value(request.arguments)
                    .map_err(|e| format!("invalid arguments: {e}"))
                    .and_then(|params| tools::execute_git_branches(&request.cwd, params)),
                "git_remotes" => serde_json::from_value(request.arguments)
                    .map_err(|e| format!("invalid arguments: {e}"))
                    .and_then(|params| tools::execute_git_remotes(&request.cwd, params)),
                other => Err(format!("unknown git tool: {other}")),
            };
            let output = result?;
            Ok(CatalogToolResult {
                output,
                success: Some(true),
                effects: Vec::new(),
            })
        })
    }
}

fn git_tool_driver() -> Arc<dyn CatalogToolDriver> {
    Arc::new(GitToolDriver)
}

inventory::submit! {
    CatalogRegistration {
        name: "git",
        tools: || tool_infos_to_catalog_tools(tools::tool_infos()),
        resources: || vec![],
        resource_templates: || vec![],
        prompts: || vec![],
        tool_driver: Some(git_tool_driver),
    }
}

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
