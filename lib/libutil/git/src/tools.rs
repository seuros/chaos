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
mod tests {
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::GitAddParams;
    use super::GitBlameParams;
    use super::GitCommitParams;
    use super::GitDiffParams;
    use super::GitShowParams;
    use super::execute_cancellable_blocking;
    use super::execute_git_add_structured;
    use super::execute_git_blame_structured;
    use super::execute_git_commit_structured;
    use super::execute_git_diff_structured_with_cancel;
    use super::execute_git_show_structured;
    use crate::BlameLine;
    use crate::CommitTrailer;
    use crate::DiffFormat;
    use crate::DiffScope;
    use crate::ShowEntry;

    fn execute_git_diff_structured(
        cwd: &Path,
        params: GitDiffParams,
    ) -> Result<serde_json::Value, String> {
        execute_git_diff_structured_with_cancel(cwd, params, Arc::new(AtomicBool::new(false)))
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("failed to run git");
        assert!(
            status.success(),
            "git command failed: git {}",
            args.join(" ")
        );
    }

    fn git_output(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git command failed: git {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git output is utf8")
    }

    #[test]
    fn execute_git_diff_returns_scoped_structured_formats_and_checks() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);

        let file = dir.join("file.txt");
        fs::write(&file, "one\ntwo\n").expect("write initial file");
        git(dir, &["add", "file.txt"]);
        git(dir, &["commit", "-m", "initial"]);

        fs::write(&file, "one\nstaged\n").expect("write staged file");
        git(dir, &["add", "file.txt"]);
        fs::write(&file, "one\nworktree  \n").expect("write worktree file");

        let staged = execute_git_diff_structured(
            dir,
            GitDiffParams {
                scope: DiffScope::Staged,
                format: DiffFormat::Patch,
                check: false,
                base: None,
                paths: Some(vec!["file.txt".to_string()]),
            },
        )
        .expect("staged patch");
        let staged_patch = staged["result"]["files"][0]["patch"]
            .as_str()
            .expect("staged patch text");
        assert!(staged_patch.contains("--- a/file.txt"));
        assert!(staged_patch.contains("+++ b/file.txt"));
        assert!(staged_patch.contains("-two"));
        assert!(staged_patch.contains("+staged"));
        assert!(!staged_patch.contains("worktree"));

        let worktree = execute_git_diff_structured(
            dir,
            GitDiffParams {
                scope: DiffScope::Worktree,
                format: DiffFormat::Stat,
                check: false,
                base: None,
                paths: None,
            },
        )
        .expect("worktree stat");
        assert_eq!(worktree["scope"], "worktree");
        assert_eq!(worktree["summary"]["files_changed"], 1);
        assert_eq!(worktree["result"]["format"], "stat");
        assert_eq!(worktree["result"]["files"][0]["path"], "file.txt");
        assert_eq!(worktree["result"]["files"][0]["additions"], 1);
        assert_eq!(worktree["result"]["files"][0]["deletions"], 1);

        let all = execute_git_diff_structured(
            dir,
            GitDiffParams {
                scope: DiffScope::All,
                format: DiffFormat::NameOnly,
                check: true,
                base: None,
                paths: None,
            },
        )
        .expect("all changed paths");
        assert_eq!(all["base"], "HEAD");
        assert_eq!(all["result"]["format"], "name_only");
        assert_eq!(all["result"]["paths"], serde_json::json!(["file.txt"]));
        assert_eq!(all["whitespace_check"]["passed"], false);
        assert_eq!(
            all["whitespace_check"]["errors"][0]["kind"],
            "trailing_whitespace"
        );
        assert_eq!(all["whitespace_check"]["errors"][0]["line"], 2);

        let error = execute_git_diff_structured(
            dir,
            GitDiffParams {
                scope: DiffScope::Worktree,
                format: DiffFormat::NameOnly,
                check: false,
                base: Some("HEAD".to_string()),
                paths: None,
            },
        )
        .expect_err("worktree base must be rejected");
        assert!(error.contains("base cannot be used with worktree scope"));
    }

    #[test]
    fn execute_git_diff_name_only_skips_oversized_blob_content() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        let large = vec![b'x'; 9 * 1024 * 1024];
        fs::write(dir.join("generated.txt"), large).expect("write large file");
        git(dir, &["add", "generated.txt"]);

        let names = execute_git_diff_structured(
            dir,
            GitDiffParams {
                scope: DiffScope::Staged,
                format: DiffFormat::NameOnly,
                check: false,
                base: None,
                paths: None,
            },
        )
        .expect("name-only should not load blob content");
        assert_eq!(
            names["result"]["paths"],
            serde_json::json!(["generated.txt"])
        );
        assert!(
            !names["summary"]
                .as_object()
                .expect("summary object")
                .contains_key("insertions")
        );

        let error = execute_git_diff_structured(
            dir,
            GitDiffParams {
                scope: DiffScope::Staged,
                format: DiffFormat::Patch,
                check: false,
                base: None,
                paths: None,
            },
        )
        .expect_err("patch generation must reject oversized content");
        assert!(error.contains("per-file limit"));
        assert!(error.contains("generated.txt"));
    }

    #[test]
    fn execute_git_diff_all_filters_staged_changes_undone_in_worktree() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        fs::write(dir.join("file.txt"), "head\n").expect("write initial file");
        git(dir, &["add", "file.txt"]);
        git(dir, &["commit", "-m", "initial"]);

        fs::write(dir.join("file.txt"), "staged\n").expect("write staged content");
        git(dir, &["add", "file.txt"]);
        fs::write(dir.join("file.txt"), "head\n").expect("restore head content");

        let all = execute_git_diff_structured(
            dir,
            GitDiffParams {
                scope: DiffScope::All,
                format: DiffFormat::NameOnly,
                check: false,
                base: None,
                paths: None,
            },
        )
        .expect("all diff");
        assert_eq!(all["result"]["paths"], serde_json::json!([]));
        assert_eq!(all["summary"]["files_changed"], 0);
    }

    #[test]
    fn cancellable_blocking_waits_for_worker_shutdown_after_timeout() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);

        let result = runtime.block_on(execute_cancellable_blocking(
            PathBuf::from("."),
            (),
            Duration::from_millis(20),
            "test operation",
            move |_, (), cancel| {
                while !cancel.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                worker_stopped.store(true, Ordering::Release);
                Ok::<(), String>(())
            },
        ));

        assert_eq!(
            result.expect_err("operation must time out"),
            "test operation timed out after 20ms"
        );
        assert!(
            stopped.load(Ordering::Acquire),
            "timeout must not leave blocking work running"
        );
    }

    #[test]
    fn execute_git_blame_includes_head_author_placeholder() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);

        let file = dir.join("file.txt");
        fs::write(&file, "alpha\nbeta\n").expect("write file");
        git(dir, &["add", "file.txt"]);
        git(dir, &["commit", "-m", "initial"]);

        let blame_json = execute_git_blame_structured(
            dir,
            GitBlameParams {
                file_path: "file.txt".to_string(),
                start_line: Some(1),
                end_line: Some(1),
            },
        )
        .expect("blame");

        let blamed: Vec<BlameLine> = serde_json::from_value(blame_json).expect("parse blame json");
        assert_eq!(blamed.len(), 1);
        assert_eq!(blamed[0].author, "Test User");
        assert_eq!(blamed[0].content, "alpha");
        assert!(!blamed[0].sha.is_empty());
    }

    #[test]
    fn execute_git_show_returns_subject_body_and_trailers() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);

        let file = dir.join("file.txt");
        fs::write(&file, "alpha\n").expect("write file");
        git(dir, &["add", "file.txt"]);
        git(
            dir,
            &[
                "commit",
                "-m",
                "feat: roast engine online",
                "-m",
                "Claude wrote a commit body with all the charisma of a tax form.\n\nSigned-off-by: Test User <test@example.com>",
            ],
        );

        let show_json = execute_git_show_structured(
            dir,
            GitShowParams {
                rev: Some("HEAD".to_string()),
            },
        )
        .expect("show");

        let shown: ShowEntry = serde_json::from_value(show_json).expect("parse show json");
        assert_eq!(shown.subject, "feat: roast engine online");
        assert!(shown.body.contains("charisma of a tax form"));
        assert_eq!(shown.author, "Test User");
        assert_eq!(shown.trailers.len(), 1);
        assert_eq!(shown.trailers[0].token, "Signed-off-by");
        assert_eq!(shown.trailers[0].value, "Test User <test@example.com>");
    }

    #[test]
    fn execute_git_add_and_commit_create_initial_and_followup_commits() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);

        let file = dir.join("file.txt");
        fs::write(&file, "alpha\n").expect("write file");

        let added = execute_git_add_structured(
            dir,
            GitAddParams {
                paths: vec!["file.txt".to_string()],
            },
        )
        .expect("add");
        assert_eq!(added["staged"], serde_json::json!(["file.txt"]));
        assert_eq!(added["removed"], serde_json::json!([]));

        let status = crate::status(dir).expect("status");
        assert_eq!(status.staged.len(), 1);
        assert_eq!(status.staged[0].path, "file.txt");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let hook = dir.join(".git/hooks/pre-commit");
            fs::create_dir_all(hook.parent().expect("hook parent")).expect("create hooks dir");
            fs::write(
                &hook,
                "#!/bin/sh\necho hook-ran > \"$PWD/hook-ran\"\nexit 1\n",
            )
            .expect("write hook");
            let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&hook, permissions).expect("make hook executable");
        }

        let committed = execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "initial commit".to_string(),
                trailers: vec![],
                amend: false,
            },
        )
        .expect("commit");
        assert_eq!(committed["operation"], "create");
        assert_eq!(committed["previous_sha"], serde_json::Value::Null);
        assert_eq!(committed["subject"], "initial commit");
        assert_eq!(committed["trailers"], serde_json::json!([]));
        assert_eq!(
            committed["committed_paths"],
            serde_json::json!(["file.txt"])
        );
        assert!(!committed["sha"].as_str().unwrap_or_default().is_empty());
        assert_eq!(git_output(dir, &["show", "HEAD:file.txt"]), "alpha\n");
        assert!(!dir.join("hook-ran").exists(), "Git hooks must not run");

        fs::write(&file, "beta\n").expect("modify tracked file");
        fs::write(dir.join("untracked.txt"), "leave me out\n").expect("write untracked file");

        execute_git_add_structured(
            dir,
            GitAddParams {
                paths: vec!["file.txt".to_string()],
            },
        )
        .expect("stage tracked file");
        execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "update tracked file".to_string(),
                trailers: vec![],
                amend: false,
            },
        )
        .expect("followup commit");

        assert_eq!(git_output(dir, &["show", "HEAD:file.txt"]), "beta\n");
        assert!(
            git_output(dir, &["status", "--porcelain"]).contains("?? untracked.txt"),
            "unrequested files must remain untracked"
        );

        let empty = execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "empty".to_string(),
                trailers: vec![],
                amend: false,
            },
        )
        .expect_err("empty commit must fail");
        assert!(empty.contains("nothing staged to commit"));
    }

    #[test]
    fn execute_git_commit_appends_structured_trailers_and_returns_them() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        fs::write(dir.join("file.txt"), "alpha\n").expect("write file");
        git(dir, &["add", "file.txt"]);

        let committed = execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: " Implement commit trailers\n\nKeep the API structured. ".to_string(),
                trailers: vec![
                    CommitTrailer {
                        token: " Signed-off-by ".to_string(),
                        value: " Mira Tenner <mira-agent@agentmail.to> ".to_string(),
                    },
                    CommitTrailer {
                        token: "Co-authored-by".to_string(),
                        value: "Daniel Tenner <daniel@tenner.org>".to_string(),
                    },
                ],
                amend: false,
            },
        )
        .expect("commit with trailers");

        assert_eq!(committed["operation"], "create");
        assert_eq!(committed["previous_sha"], serde_json::Value::Null);
        assert_eq!(
            committed["trailers"],
            serde_json::json!([
                {
                    "token": "Signed-off-by",
                    "value": "Mira Tenner <mira-agent@agentmail.to>"
                },
                {
                    "token": "Co-authored-by",
                    "value": "Daniel Tenner <daniel@tenner.org>"
                }
            ])
        );
        assert_eq!(
            git_output(dir, &["show", "-s", "--format=%B", "HEAD"]),
            concat!(
                "Implement commit trailers\n\n",
                "Keep the API structured.\n\n",
                "Signed-off-by: Mira Tenner <mira-agent@agentmail.to>\n",
                "Co-authored-by: Daniel Tenner <daniel@tenner.org>\n"
            )
        );

        let shown = crate::show(dir, None).expect("show committed trailers");
        assert_eq!(
            shown.trailers,
            vec![
                CommitTrailer {
                    token: "Signed-off-by".to_string(),
                    value: "Mira Tenner <mira-agent@agentmail.to>".to_string(),
                },
                CommitTrailer {
                    token: "Co-authored-by".to_string(),
                    value: "Daniel Tenner <daniel@tenner.org>".to_string(),
                },
            ]
        );
    }

    #[test]
    fn execute_git_commit_rejects_invalid_structured_trailers() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        fs::write(dir.join("file.txt"), "alpha\n").expect("write file");
        git(dir, &["add", "file.txt"]);

        let invalid = [
            (
                CommitTrailer {
                    token: "Signed off by".to_string(),
                    value: "Test User <test@example.com>".to_string(),
                },
                "invalid commit trailer token",
            ),
            (
                CommitTrailer {
                    token: "Signed-off-by".to_string(),
                    value: "Test User\nInjected-by: Someone".to_string(),
                },
                "must be a single line",
            ),
            (
                CommitTrailer {
                    token: "Signed-off-by".to_string(),
                    value: "   ".to_string(),
                },
                "must not be empty",
            ),
        ];

        for (trailer, expected) in invalid {
            let error = execute_git_commit_structured(
                dir,
                GitCommitParams {
                    message: "invalid trailer".to_string(),
                    trailers: vec![trailer],
                    amend: false,
                },
            )
            .expect_err("invalid trailer must be rejected");
            assert!(error.contains(expected), "unexpected error: {error}");
        }

        assert_eq!(
            git_output(dir, &["rev-list", "--count", "--all"]),
            "0\n",
            "invalid trailers must not create a commit"
        );
    }

    #[test]
    fn execute_git_add_stages_deletions_and_rejects_unsafe_paths() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);

        fs::write(dir.join("tracked.txt"), "tracked\n").expect("write tracked file");
        git(dir, &["add", "tracked.txt"]);
        git(dir, &["commit", "-m", "initial"]);

        fs::remove_file(dir.join("tracked.txt")).expect("remove tracked file");
        let deleted = execute_git_add_structured(
            dir,
            GitAddParams {
                paths: vec!["tracked.txt".to_string()],
            },
        )
        .expect("stage deletion");
        assert_eq!(deleted["removed"], serde_json::json!(["tracked.txt"]));

        fs::write(dir.join(".gitignore"), "*.log\n").expect("write ignore file");
        fs::write(dir.join("ignored.log"), "ignored\n").expect("write ignored file");
        let ignored = execute_git_add_structured(
            dir,
            GitAddParams {
                paths: vec!["ignored.log".to_string()],
            },
        )
        .expect_err("ignored file must fail");
        assert!(ignored.contains("path is ignored: ignored.log"));

        let traversal = execute_git_add_structured(
            dir,
            GitAddParams {
                paths: vec!["../outside.txt".to_string()],
            },
        )
        .expect_err("path traversal must fail");
        assert!(traversal.contains("may not escape the repository"));

        let directory = execute_git_add_structured(
            dir,
            GitAddParams {
                paths: vec![".git".to_string()],
            },
        )
        .expect_err("git directory must fail");
        assert!(directory.contains("Git directory"));
    }

    #[test]
    fn execute_git_add_paths_are_relative_to_repository_root() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        fs::create_dir(dir.join("nested")).expect("create nested directory");
        fs::write(dir.join("root.txt"), "root\n").expect("write root file");

        let added = execute_git_add_structured(
            &dir.join("nested"),
            GitAddParams {
                paths: vec!["root.txt".to_string()],
            },
        )
        .expect("stage root-relative path from nested cwd");

        assert_eq!(added["staged"], serde_json::json!(["root.txt"]));
        assert_eq!(
            git_output(dir, &["diff", "--cached", "--name-only"]),
            "root.txt\n"
        );
    }

    #[test]
    fn execute_git_commit_preserves_unchanged_gitlinks_and_rejects_changes() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);

        fs::write(dir.join("file.txt"), "alpha\n").expect("write file");
        git(dir, &["add", "file.txt"]);
        git(dir, &["commit", "-m", "initial"]);

        let gitlink_id = git_output(dir, &["rev-parse", "HEAD"]).trim().to_string();
        git(
            dir,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000",
                &gitlink_id,
                "vendor/reference",
            ],
        );
        git(dir, &["commit", "-m", "add gitlink"]);

        fs::write(dir.join("file.txt"), "beta\n").expect("modify file");
        execute_git_add_structured(
            dir,
            GitAddParams {
                paths: vec!["file.txt".to_string()],
            },
        )
        .expect("stage file");
        execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "update file".to_string(),
                trailers: vec![],
                amend: false,
            },
        )
        .expect("commit while preserving gitlink");

        assert!(
            git_output(dir, &["ls-tree", "HEAD", "vendor/reference"]).contains(&gitlink_id),
            "unchanged gitlink must be preserved"
        );

        let changed_gitlink_id = git_output(dir, &["rev-parse", "HEAD"]).trim().to_string();
        git(
            dir,
            &[
                "update-index",
                "--cacheinfo",
                "160000",
                &changed_gitlink_id,
                "vendor/reference",
            ],
        );
        let error = execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "change gitlink".to_string(),
                trailers: vec![],
                amend: false,
            },
        )
        .expect_err("staged gitlink change must fail");
        assert!(error.contains("staged submodule changes are not supported: vendor/reference"));
    }

    #[test]
    fn execute_git_commit_amends_message_and_includes_staged_changes() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        fs::write(dir.join("file.txt"), "alpha\n").expect("write file");
        git(dir, &["add", "file.txt"]);
        git(dir, &["commit", "-m", "initial"]);
        fs::write(dir.join("second.txt"), "second\n").expect("write second file");
        git(dir, &["add", "second.txt"]);
        git(dir, &["commit", "-m", "second"]);

        let original_sha = git_output(dir, &["rev-parse", "HEAD"]).trim().to_string();
        let original_parent = git_output(dir, &["rev-parse", "HEAD^"]).trim().to_string();
        let original_author = git_output(dir, &["show", "-s", "--format=%an|%ae|%at", "HEAD"]);

        let message_only = execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "second, revised".to_string(),
                trailers: vec![],
                amend: true,
            },
        )
        .expect("amend message");

        assert_eq!(message_only["operation"], "amend");
        assert_eq!(
            message_only["previous_sha"].as_str(),
            Some(original_sha.as_str())
        );
        assert_eq!(message_only["subject"], "second, revised");
        assert_eq!(message_only["trailers"], serde_json::json!([]));
        assert_eq!(message_only["committed_paths"], serde_json::json!([]));
        assert_ne!(
            message_only["sha"].as_str().expect("amended sha"),
            original_sha
        );
        assert_eq!(
            git_output(dir, &["rev-parse", "HEAD^"]).trim(),
            original_parent
        );
        assert_eq!(
            git_output(dir, &["show", "-s", "--format=%an|%ae|%at", "HEAD"]),
            original_author,
            "amend must preserve the original author"
        );
        assert_eq!(git_output(dir, &["rev-list", "--count", "HEAD"]), "2\n");

        fs::write(dir.join("second.txt"), "updated\n").expect("update second file");
        execute_git_add_structured(
            dir,
            GitAddParams {
                paths: vec!["second.txt".to_string()],
            },
        )
        .expect("stage amended content");
        let with_changes = execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "second, revised again".to_string(),
                trailers: vec![],
                amend: true,
            },
        )
        .expect("amend with staged change");

        assert_eq!(
            with_changes["committed_paths"],
            serde_json::json!(["second.txt"])
        );
        assert_eq!(git_output(dir, &["show", "HEAD:second.txt"]), "updated\n");
        assert_eq!(
            git_output(dir, &["rev-parse", "HEAD^"]).trim(),
            original_parent
        );
        assert_eq!(git_output(dir, &["rev-list", "--count", "HEAD"]), "2\n");

        git(dir, &["checkout", "--detach", "HEAD"]);
        let detached = execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "detached revision".to_string(),
                trailers: vec![],
                amend: true,
            },
        )
        .expect("amend detached HEAD");
        assert_eq!(detached["detached"], true);
        assert_eq!(detached["branch"], serde_json::Value::Null);
        assert_eq!(
            git_output(dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "HEAD\n"
        );
        assert_eq!(
            git_output(dir, &["rev-parse", "HEAD^"]).trim(),
            original_parent
        );
    }

    #[test]
    fn execute_git_commit_rejects_amend_without_head_commit() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();
        git(dir, &["init"]);

        let error = execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "cannot exist".to_string(),
                trailers: vec![],
                amend: true,
            },
        )
        .expect_err("unborn HEAD must not be amendable");

        assert!(error.contains("cannot amend because HEAD has no commit"));
    }
}
