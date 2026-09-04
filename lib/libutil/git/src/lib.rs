//! `chaos-git` — pure-Rust git inspection, staging, and commits.
//!
//! Provides structured access to repository state without shelling out to
//! `git(1)`. Built on `gix` (gitoxide).
//!
//! ## MCP surface
//!
//! **Resources** (implicit cwd):
//! - `git://branches{?scope,contains}` — current, default, local, and remote branches
//!
//! **Tools** (require params):
//! - `git_diff` — scoped structured patches, statistics, changed paths, and checks
//! - `log` — commit history with optional limit and branch
//! - `show` — full commit details with subject, body, and trailers
//! - `blame` — per-line attribution for a file
//! - `add` — stage explicit repository-relative file paths
//! - `commit` — create or amend an unsigned commit without hooks
//! - `branch` — create or delete a local branch

mod add;
mod blame;
mod branch;
mod branches;
mod commit;
mod diff;
mod error;
mod ext;
mod log;
mod remotes;
mod repo;
mod resources;
mod show;
mod status;
mod tools;

pub use add::AddResult;
pub use add::add;
pub use blame::BlameLine;
pub use blame::blame;
pub use branch::BranchMutationResult;
pub use branch::create as create_branch;
pub use branch::delete as delete_branch;
pub use branches::BranchInfo;
use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::CatalogResourceDriver;
use chaos_traits::catalog::CatalogResourceDriverRegistration;
use chaos_traits::catalog::CatalogResourceTemplate;
use chaos_traits::catalog::CatalogTool;
use chaos_traits::catalog::CatalogToolDriver;
use chaos_traits::catalog::CatalogToolDriverFuture;
use chaos_traits::catalog::CatalogToolRequest;
use chaos_traits::catalog::CatalogToolResult;
use chaos_traits::catalog::ToolExposure;
use chaos_traits::catalog::tool_infos_to_catalog_tools;
pub use commit::CommitResult;
pub use commit::amend;
pub use commit::commit;
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
            let cwd = request.cwd;
            let result = match request.tool_name.as_str() {
                "git_diff" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_git_diff_blocking(cwd, params).await
                }
                "git_log" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_blocking(cwd, params, tools::execute_git_log_structured).await
                }
                "git_show" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_blocking(cwd, params, tools::execute_git_show_structured).await
                }
                "git_blame" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_blocking(cwd, params, tools::execute_git_blame_structured).await
                }
                "git_repo" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_blocking(cwd, params, tools::execute_git_repo_structured).await
                }
                "git_status" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_blocking(cwd, params, tools::execute_git_status_structured).await
                }
                "git_remotes" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_blocking(cwd, params, tools::execute_git_remotes_structured)
                        .await
                }
                "git_add" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_blocking(cwd, params, tools::execute_git_add_structured).await
                }
                "git_commit" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_blocking(cwd, params, tools::execute_git_commit_structured).await
                }
                "git_branch" => {
                    let params = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::execute_blocking(cwd, params, tools::execute_git_branch_structured).await
                }
                other => Err(format!("unknown git tool: {other}")),
            };
            let output = result?.to_string();
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

fn git_catalog_tools() -> Vec<CatalogTool> {
    tool_infos_to_catalog_tools(tools::tool_infos())
        .into_iter()
        .map(|mut tool| {
            if matches!(tool.name.as_str(), "git_add" | "git_commit" | "git_branch") {
                tool.read_only_hint = Some(false);
                tool.supports_parallel_tool_calls = false;
            }
            tool
        })
        .collect()
}

fn git_tool_exposure(tool_name: &str) -> ToolExposure {
    if matches!(tool_name, "git_add" | "git_commit" | "git_branch") {
        ToolExposure::groups(["git-write"])
    } else {
        ToolExposure::groups(["git"])
    }
}

fn git_resource_templates() -> Vec<CatalogResourceTemplate> {
    vec![CatalogResourceTemplate {
        uri_template: "git://branches{?scope,contains}".to_string(),
        name: "git_branches".to_string(),
        description: Some(
            "List local and remote branches, optionally filtered by scope and substring"
                .to_string(),
        ),
        mime_type: Some("application/json".to_string()),
    }]
}

fn git_resource_driver() -> Arc<dyn CatalogResourceDriver> {
    Arc::new(resources::GitResourceDriver)
}

fn git_resource_exposure(_uri: &str) -> ToolExposure {
    ToolExposure::groups(["git"])
}

inventory::submit! {
    CatalogRegistration {
        name: "git",
        tools: git_catalog_tools,
        resources: || vec![],
        resource_templates: git_resource_templates,
        prompts: || vec![],
        tool_driver: Some(git_tool_driver),
        tool_exposure: git_tool_exposure,
    }
}

inventory::submit! {
    CatalogResourceDriverRegistration {
        module: "git",
        driver: git_resource_driver,
        exposure: git_resource_exposure,
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

#[cfg(test)]
mod tests {
    #[test]
    fn mutation_tools_are_marked_mutating_and_non_parallel() {
        let tools = super::git_catalog_tools();
        for name in ["git_add", "git_commit", "git_branch"] {
            let tool = tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(tool.read_only_hint, Some(false));
            assert!(!tool.supports_parallel_tool_calls);
        }

        let status = tools
            .iter()
            .find(|tool| tool.name == "git_status")
            .expect("git_status");
        assert_eq!(status.read_only_hint, Some(true));
        assert!(status.supports_parallel_tool_calls);
        assert!(
            tools.iter().all(|tool| tool.name != "git_branches"),
            "branch listing must be exposed as a resource, not a tool"
        );
    }

    #[test]
    fn git_diff_schema_requires_structured_scope_format_and_check() {
        let tools = super::git_catalog_tools();
        let diff = tools
            .iter()
            .find(|tool| tool.name == "git_diff")
            .expect("git_diff");
        let required = diff.input_schema["required"]
            .as_array()
            .expect("required properties");

        for name in ["scope", "format", "check"] {
            assert!(
                required.iter().any(|value| value == name),
                "{name} must be required: {}",
                diff.input_schema
            );
        }
        for name in ["base", "paths"] {
            assert!(
                required.iter().all(|value| value != name),
                "{name} must remain optional: {}",
                diff.input_schema
            );
        }
    }

    #[test]
    fn git_commit_schema_exposes_optional_destructive_amend() {
        let commit = super::tools::tool_infos()
            .into_iter()
            .find(|tool| tool.name == "git_commit")
            .expect("git_commit");
        let required = commit.input_schema["required"]
            .as_array()
            .expect("required properties");

        assert!(required.iter().any(|value| value == "message"));
        assert!(required.iter().all(|value| value != "amend"));
        assert_eq!(
            commit.input_schema["properties"]["amend"]["type"],
            "boolean"
        );
        let destructive = commit
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.destructive_hint)
            .unwrap_or(true);
        assert!(destructive);
    }

    #[test]
    fn branch_resource_template_is_registered() {
        let templates = super::git_resource_templates();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].uri_template, "git://branches{?scope,contains}");
        assert_eq!(templates[0].mime_type.as_deref(), Some("application/json"));
    }
}
