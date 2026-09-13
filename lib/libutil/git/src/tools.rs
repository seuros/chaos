use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::GitCtx;
use crate::GitServer;

const GIT_TOOL_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn execute_blocking<P, F, R>(cwd: PathBuf, params: P, f: F) -> Result<R, String>
where
    P: Send + 'static,
    F: FnOnce(&Path, P) -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let task = tokio::task::spawn_blocking(move || f(&cwd, params));
    tokio::time::timeout(GIT_TOOL_TIMEOUT, task)
        .await
        .map_err(|_| "git tool timed out".to_string())?
        .map_err(|e| format!("git tool task failed: {e}"))?
}

async fn execute_cancellable_blocking<P, F, R>(
    cwd: PathBuf,
    params: P,
    timeout: Duration,
    operation: &'static str,
    f: F,
) -> Result<R, String>
where
    P: Send + 'static,
    F: FnOnce(&Path, P, Arc<AtomicBool>) -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let mut task = tokio::task::spawn_blocking(move || f(&cwd, params, worker_cancel));
    tokio::select! {
        biased;
        result = &mut task => {
            result.map_err(|e| format!("{operation} task failed: {e}"))?
        }
        _ = tokio::time::sleep(timeout) => {
            cancel.store(true, Ordering::Release);
            let _worker_result = task.await.map_err(|e| {
                format!("{operation} timed out after {timeout:?}; worker cleanup failed: {e}")
            })?;
            Err(format!("{operation} timed out after {timeout:?}"))
        }
    }
}

pub(crate) async fn execute_git_diff_blocking(
    cwd: PathBuf,
    params: GitDiffParams,
) -> Result<serde_json::Value, String> {
    execute_cancellable_blocking(
        cwd,
        params,
        GIT_TOOL_TIMEOUT,
        "git diff",
        execute_git_diff_structured_with_cancel,
    )
    .await
}

fn output_from_json_result(result: Result<serde_json::Value, String>) -> ToolResult {
    match result {
        Ok(value) => ToolOutput::structured(value)
            .map_err(|e| ToolError::Execution(format!("non-object tool output: {e}"))),
        Err(msg) => Err(ToolError::Execution(msg)),
    }
}

fn to_json_value<T: serde::Serialize>(value: T) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn default_log_limit() -> usize {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitDiffParams {
    /// Comparison scope: worktree is index-to-filesystem, staged is base-to-index,
    /// and all is base-to-filesystem. Untracked files are excluded.
    scope: crate::DiffScope,
    /// Output representation.
    format: crate::DiffFormat,
    /// Check newly added lines for whitespace errors and conflict markers.
    check: bool,
    /// Optional base ref for staged or all scope (default: HEAD). Invalid with worktree.
    #[serde(default)]
    base: Option<String>,
    /// Optional exact file or directory-prefix filters relative to repo root.
    #[serde(default)]
    paths: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitLogParams {
    /// Maximum number of entries to return.
    #[serde(default = "default_log_limit")]
    limit: usize,
    /// Optional ref to walk from (default: HEAD).
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitShowParams {
    /// Revision to show (default: HEAD).
    #[serde(default)]
    rev: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitShowFileParams {
    /// File path relative to repo root.
    file_path: String,
    /// Revision to read (default: HEAD).
    #[serde(default)]
    rev: Option<String>,
    /// Optional 1-indexed start line, inclusive.
    #[serde(default)]
    start_line: Option<usize>,
    /// Optional 1-indexed end line, inclusive.
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitBlameParams {
    /// File path relative to repo root.
    file_path: String,
    /// Optional 1-indexed start line, inclusive.
    #[serde(default)]
    start_line: Option<usize>,
    /// Optional 1-indexed end line, inclusive.
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitRepoParams {}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitStatusParams {}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitRemotesParams {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitAddParams {
    /// Explicit repository-relative file paths to stage.
    paths: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitCommitParams {
    /// Commit message. The first line becomes the commit subject.
    message: String,
    /// Structured trailers appended to the stored commit message in order.
    #[serde(default)]
    trailers: Vec<crate::CommitTrailer>,
    /// Replace HEAD instead of creating a child commit. Staged changes, if any,
    /// are included; with no staged changes, only the commit message changes.
    #[serde(default)]
    amend: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GitBranchOperation {
    Create,
    Delete,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitBranchParams {
    /// Branch operation to perform.
    operation: GitBranchOperation,
    /// Short local branch name, without the refs/heads/ prefix.
    name: String,
    /// Revision used as the new branch tip. Create only; defaults to HEAD.
    #[serde(default)]
    start_point: Option<String>,
    /// Allow deletion when the branch is not merged into HEAD. Delete only.
    #[serde(default)]
    force: bool,
}

impl GitServer {
    #[mcp_tool(
        name = "git_diff",
        description = "Inspect tracked changes by scope, returning structured patches, statistics, or changed paths with optional whitespace checks.",
        read_only = true,
        open_world = false
    )]
    async fn git_diff(&self, _ctx: GitCtx<'_>, params: Parameters<GitDiffParams>) -> ToolResult {
        output_from_json_result(execute_git_diff_blocking(PathBuf::from("."), params.0).await)
    }

    #[mcp_tool(
        name = "git_log",
        description = "List recent commits with sha, author, date, and subject.",
        read_only = true,
        open_world = false
    )]
    async fn git_log(&self, _ctx: GitCtx<'_>, params: Parameters<GitLogParams>) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_log_structured).await,
        )
    }

    #[mcp_tool(
        name = "git_show",
        description = "Show full commit details including subject, body, author, and trailers.",
        read_only = true,
        open_world = false
    )]
    async fn git_show(&self, _ctx: GitCtx<'_>, params: Parameters<GitShowParams>) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_show_structured).await,
        )
    }

    #[mcp_tool(
        name = "git_show_file",
        description = "Show a tracked file at a revision, with optional line range. Untracked worktree edits are ignored.",
        read_only = true,
        open_world = false
    )]
    async fn git_show_file(
        &self,
        _ctx: GitCtx<'_>,
        params: Parameters<GitShowFileParams>,
    ) -> ToolResult {
        output_from_json_result(
            execute_blocking(
                PathBuf::from("."),
                params.0,
                execute_git_show_file_structured,
            )
            .await,
        )
    }

    #[mcp_tool(
        name = "git_blame",
        description = "Show per-line author attribution for a file, with optional line range.",
        read_only = true,
        open_world = false
    )]
    async fn git_blame(&self, _ctx: GitCtx<'_>, params: Parameters<GitBlameParams>) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_blame_structured).await,
        )
    }

    #[mcp_tool(
        name = "git_repo",
        description = "Show repository identity: root path, HEAD sha, current branch, remotes, and dirty state.",
        read_only = true,
        open_world = false
    )]
    async fn git_repo(&self, _ctx: GitCtx<'_>, params: Parameters<GitRepoParams>) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_repo_structured).await,
        )
    }

    #[mcp_tool(
        name = "git_status",
        description = "List staged, unstaged, and untracked files in the worktree.",
        read_only = true,
        open_world = false
    )]
    async fn git_status(
        &self,
        _ctx: GitCtx<'_>,
        params: Parameters<GitStatusParams>,
    ) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_status_structured).await,
        )
    }

    #[mcp_tool(
        name = "git_remotes",
        description = "List configured remotes with their fetch and push URLs.",
        read_only = true,
        open_world = false
    )]
    async fn git_remotes(
        &self,
        _ctx: GitCtx<'_>,
        params: Parameters<GitRemotesParams>,
    ) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_remotes_structured).await,
        )
    }

    #[mcp_tool(
        name = "git_add",
        description = "Stage explicit repository-relative files or deletions. Directories, ignored new files, conflicts, and submodules are rejected.",
        read_only = false,
        destructive = false,
        open_world = false
    )]
    async fn git_add(&self, _ctx: GitCtx<'_>, params: Parameters<GitAddParams>) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_add_structured).await,
        )
    }

    #[mcp_tool(
        name = "git_commit",
        description = "Create or amend an unsigned commit, with optional structured trailers. Git hooks are not run.",
        read_only = false,
        destructive = true,
        open_world = false
    )]
    async fn git_commit(
        &self,
        _ctx: GitCtx<'_>,
        params: Parameters<GitCommitParams>,
    ) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_commit_structured).await,
        )
    }

    #[mcp_tool(
        name = "git_branch",
        description = "Create or delete a local branch. Creation does not check out the branch; deletion rejects checked-out or unmerged branches unless force is set.",
        read_only = false,
        destructive = true,
        open_world = false
    )]
    async fn git_branch(
        &self,
        _ctx: GitCtx<'_>,
        params: Parameters<GitBranchParams>,
    ) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_branch_structured).await,
        )
    }
}

pub fn tool_infos() -> Vec<ToolInfo> {
    vec![
        GitServer::git_diff_tool_info(),
        GitServer::git_log_tool_info(),
        GitServer::git_show_tool_info(),
        GitServer::git_show_file_tool_info(),
        GitServer::git_blame_tool_info(),
        GitServer::git_repo_tool_info(),
        GitServer::git_status_tool_info(),
        GitServer::git_remotes_tool_info(),
        GitServer::git_add_tool_info(),
        GitServer::git_commit_tool_info(),
        GitServer::git_branch_tool_info(),
    ]
}

fn execute_git_diff_structured_with_cancel(
    cwd: &Path,
    params: GitDiffParams,
    cancel: Arc<AtomicBool>,
) -> Result<serde_json::Value, String> {
    let GitDiffParams {
        scope,
        format,
        check,
        base,
        paths,
    } = params;
    let report_scope = scope;
    let path_refs = paths
        .as_ref()
        .map(|items| items.iter().map(String::as_str).collect::<Vec<_>>());
    let report = crate::diff::diff_report_with_cancel(
        cwd,
        report_scope,
        format,
        base.as_deref(),
        path_refs.as_deref(),
        check,
        cancel,
    )
    .map_err(|e| e.to_string())?;
    let result = match format {
        crate::DiffFormat::Patch => serde_json::json!({
            "format": format,
            "files": report.files.iter().map(|file| serde_json::json!({
                "path": file.path,
                "status": file.status,
                "binary": file.binary,
                "patch": file.patch,
            })).collect::<Vec<_>>(),
        }),
        crate::DiffFormat::Stat => serde_json::json!({
            "format": format,
            "files": report.files.iter().map(|file| serde_json::json!({
                "path": file.path,
                "status": file.status,
                "binary": file.binary,
                "additions": file.additions,
                "deletions": file.deletions,
            })).collect::<Vec<_>>(),
        }),
        crate::DiffFormat::NameOnly => serde_json::json!({
            "format": format,
            "paths": report.paths,
        }),
    };
    let whitespace_check = if check {
        serde_json::json!({
            "checked": true,
            "passed": report.whitespace_errors.is_empty(),
            "errors": report.whitespace_errors,
        })
    } else {
        serde_json::json!({
            "checked": false,
            "passed": null,
            "errors": [],
        })
    };
    let resolved_base = match report_scope {
        crate::DiffScope::Worktree => None,
        crate::DiffScope::Staged | crate::DiffScope::All => Some(base.as_deref().unwrap_or("HEAD")),
    };
    Ok(serde_json::json!({
        "scope": report_scope,
        "base": resolved_base,
        "path_filters": paths.unwrap_or_default(),
        "summary": report.summary,
        "result": result,
        "whitespace_check": whitespace_check,
    }))
}

pub fn execute_git_log_structured(
    cwd: &Path,
    params: GitLogParams,
) -> Result<serde_json::Value, String> {
    let entries =
        crate::log(cwd, Some(params.limit), params.branch.as_deref()).map_err(|e| e.to_string())?;
    to_json_value(entries)
}

pub fn execute_git_show_structured(
    cwd: &Path,
    params: GitShowParams,
) -> Result<serde_json::Value, String> {
    let entry = crate::show(cwd, params.rev.as_deref()).map_err(|e| e.to_string())?;
    to_json_value(entry)
}

pub fn execute_git_show_file_structured(
    cwd: &Path,
    params: GitShowFileParams,
) -> Result<serde_json::Value, String> {
    let lines = match (params.start_line, params.end_line) {
        (Some(start), Some(end)) => Some((start, end)),
        (None, None) => None,
        _ => {
            return Err(
                "start_line and end_line must either both be provided or both be omitted"
                    .to_string(),
            );
        }
    };
    let file = crate::show_file(cwd, &params.file_path, params.rev.as_deref(), lines)
        .map_err(|e| e.to_string())?;
    to_json_value(file)
}

pub fn execute_git_blame_structured(
    cwd: &Path,
    params: GitBlameParams,
) -> Result<serde_json::Value, String> {
    let lines = match (params.start_line, params.end_line) {
        (Some(start), Some(end)) => Some((start, end)),
        (None, None) => None,
        _ => {
            return Err(
                "start_line and end_line must either both be provided or both be omitted"
                    .to_string(),
            );
        }
    };
    let blamed = crate::blame(cwd, &params.file_path, lines).map_err(|e| e.to_string())?;
    to_json_value(blamed)
}

pub fn execute_git_repo_structured(
    cwd: &Path,
    _params: GitRepoParams,
) -> Result<serde_json::Value, String> {
    let info = crate::repo_info(cwd).map_err(|e| e.to_string())?;
    to_json_value(info)
}

pub fn execute_git_status_structured(
    cwd: &Path,
    _params: GitStatusParams,
) -> Result<serde_json::Value, String> {
    let info = crate::status(cwd).map_err(|e| e.to_string())?;
    to_json_value(info)
}

pub fn execute_git_remotes_structured(
    cwd: &Path,
    _params: GitRemotesParams,
) -> Result<serde_json::Value, String> {
    let info = crate::remotes(cwd).map_err(|e| e.to_string())?;
    to_json_value(info)
}

pub fn execute_git_add_structured(
    cwd: &Path,
    params: GitAddParams,
) -> Result<serde_json::Value, String> {
    let result = crate::add(cwd, &params.paths).map_err(|e| e.to_string())?;
    to_json_value(result)
}

pub fn execute_git_commit_structured(
    cwd: &Path,
    params: GitCommitParams,
) -> Result<serde_json::Value, String> {
    let result = if params.amend {
        crate::amend_with_trailers(cwd, &params.message, &params.trailers)
    } else {
        crate::commit_with_trailers(cwd, &params.message, &params.trailers)
    }
    .map_err(|e| e.to_string())?;
    to_json_value(result)
}

pub fn execute_git_branch_structured(
    cwd: &Path,
    params: GitBranchParams,
) -> Result<serde_json::Value, String> {
    let result = match params.operation {
        GitBranchOperation::Create => {
            if params.force {
                return Err("force is only valid for branch deletion".to_string());
            }
            crate::create_branch(cwd, &params.name, params.start_point.as_deref())
        }
        GitBranchOperation::Delete => {
            if params.start_point.is_some() {
                return Err("start_point is only valid for branch creation".to_string());
            }
            crate::delete_branch(cwd, &params.name, params.force)
        }
    }
    .map_err(|error| error.to_string())?;
    to_json_value(result)
}

#[cfg(test)]
mod tests;
