use std::path::Path;
use std::path::PathBuf;
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

fn output_from_json_result(result: Result<serde_json::Value, String>) -> ToolResult {
    match result {
        Ok(value) => ToolOutput::structured(value)
            .map_err(|e| ToolError::Execution(format!("non-object tool output: {e}"))),
        Err(msg) => Err(ToolError::Execution(msg)),
    }
}

fn default_log_limit() -> usize {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GitDiffParams {
    /// Optional ref to diff against (default: HEAD).
    #[serde(default)]
    base: Option<String>,
    /// Optional path filters relative to repo root.
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
pub struct GitBranchesParams {}

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
}

impl GitServer {
    #[mcp_tool(
        name = "git_diff",
        description = "Show unified diff of worktree changes against a base ref (default HEAD).",
        read_only = true,
        open_world = false
    )]
    async fn git_diff(&self, _ctx: GitCtx<'_>, params: Parameters<GitDiffParams>) -> ToolResult {
        output_from_json_result(
            execute_blocking(PathBuf::from("."), params.0, execute_git_diff_structured).await,
        )
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
        name = "git_branches",
        description = "List local and remote branches with current and default markers.",
        read_only = true,
        open_world = false
    )]
    async fn git_branches(
        &self,
        _ctx: GitCtx<'_>,
        params: Parameters<GitBranchesParams>,
    ) -> ToolResult {
        output_from_json_result(
            execute_blocking(
                PathBuf::from("."),
                params.0,
                execute_git_branches_structured,
            )
            .await,
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
        description = "Create an unsigned commit from the staged index using configured identity. Git hooks are not run.",
        read_only = false,
        destructive = false,
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
}

pub fn tool_infos() -> Vec<ToolInfo> {
    vec![
        GitServer::git_diff_tool_info(),
        GitServer::git_log_tool_info(),
        GitServer::git_show_tool_info(),
        GitServer::git_blame_tool_info(),
        GitServer::git_repo_tool_info(),
        GitServer::git_status_tool_info(),
        GitServer::git_branches_tool_info(),
        GitServer::git_remotes_tool_info(),
        GitServer::git_add_tool_info(),
        GitServer::git_commit_tool_info(),
    ]
}

#[allow(dead_code)]
pub fn execute_git_diff(cwd: &Path, params: GitDiffParams) -> Result<String, String> {
    execute_git_diff_structured(cwd, params).map(|value| {
        value
            .get("diff")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    })
}

pub fn execute_git_diff_structured(
    cwd: &Path,
    params: GitDiffParams,
) -> Result<serde_json::Value, String> {
    let GitDiffParams { base, paths } = params;
    let path_refs = paths
        .as_ref()
        .map(|items| items.iter().map(String::as_str).collect::<Vec<_>>());
    let diff =
        crate::diff(cwd, base.as_deref(), path_refs.as_deref()).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "diff": diff,
        "base": base,
        "paths": paths,
    }))
}

#[allow(dead_code)]
pub fn execute_git_log(cwd: &Path, params: GitLogParams) -> Result<String, String> {
    execute_git_log_structured(cwd, params)
        .and_then(|value| serde_json::to_string_pretty(&value).map_err(|e| e.to_string()))
}

pub fn execute_git_log_structured(
    cwd: &Path,
    params: GitLogParams,
) -> Result<serde_json::Value, String> {
    let entries =
        crate::log(cwd, Some(params.limit), params.branch.as_deref()).map_err(|e| e.to_string())?;
    serde_json::to_value(entries).map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub fn execute_git_show(cwd: &Path, params: GitShowParams) -> Result<String, String> {
    execute_git_show_structured(cwd, params)
        .and_then(|value| serde_json::to_string_pretty(&value).map_err(|e| e.to_string()))
}

pub fn execute_git_show_structured(
    cwd: &Path,
    params: GitShowParams,
) -> Result<serde_json::Value, String> {
    let entry = crate::show(cwd, params.rev.as_deref()).map_err(|e| e.to_string())?;
    serde_json::to_value(entry).map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub fn execute_git_blame(cwd: &Path, params: GitBlameParams) -> Result<String, String> {
    execute_git_blame_structured(cwd, params)
        .and_then(|value| serde_json::to_string_pretty(&value).map_err(|e| e.to_string()))
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
    serde_json::to_value(blamed).map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub fn execute_git_repo(cwd: &Path, _params: GitRepoParams) -> Result<String, String> {
    execute_git_repo_structured(cwd, _params)
        .and_then(|value| serde_json::to_string_pretty(&value).map_err(|e| e.to_string()))
}

pub fn execute_git_repo_structured(
    cwd: &Path,
    _params: GitRepoParams,
) -> Result<serde_json::Value, String> {
    let info = crate::repo_info(cwd).map_err(|e| e.to_string())?;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub fn execute_git_status(cwd: &Path, _params: GitStatusParams) -> Result<String, String> {
    execute_git_status_structured(cwd, _params)
        .and_then(|value| serde_json::to_string_pretty(&value).map_err(|e| e.to_string()))
}

pub fn execute_git_status_structured(
    cwd: &Path,
    _params: GitStatusParams,
) -> Result<serde_json::Value, String> {
    let info = crate::status(cwd).map_err(|e| e.to_string())?;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub fn execute_git_branches(cwd: &Path, _params: GitBranchesParams) -> Result<String, String> {
    execute_git_branches_structured(cwd, _params)
        .and_then(|value| serde_json::to_string_pretty(&value).map_err(|e| e.to_string()))
}

pub fn execute_git_branches_structured(
    cwd: &Path,
    _params: GitBranchesParams,
) -> Result<serde_json::Value, String> {
    let info = crate::branches(cwd).map_err(|e| e.to_string())?;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub fn execute_git_remotes(cwd: &Path, _params: GitRemotesParams) -> Result<String, String> {
    execute_git_remotes_structured(cwd, _params)
        .and_then(|value| serde_json::to_string_pretty(&value).map_err(|e| e.to_string()))
}

pub fn execute_git_remotes_structured(
    cwd: &Path,
    _params: GitRemotesParams,
) -> Result<serde_json::Value, String> {
    let info = crate::remotes(cwd).map_err(|e| e.to_string())?;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

pub fn execute_git_add_structured(
    cwd: &Path,
    params: GitAddParams,
) -> Result<serde_json::Value, String> {
    let result = crate::add(cwd, &params.paths).map_err(|e| e.to_string())?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

pub fn execute_git_commit_structured(
    cwd: &Path,
    params: GitCommitParams,
) -> Result<serde_json::Value, String> {
    let result = crate::commit(cwd, &params.message).map_err(|e| e.to_string())?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::GitAddParams;
    use super::GitBlameParams;
    use super::GitCommitParams;
    use super::GitDiffParams;
    use super::GitShowParams;
    use super::execute_git_add_structured;
    use super::execute_git_blame;
    use super::execute_git_commit_structured;
    use super::execute_git_diff;
    use super::execute_git_show;
    use crate::BlameLine;
    use crate::ShowEntry;

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
    fn execute_git_diff_surfaces_worktree_changes_against_head() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path();

        git(dir, &["init"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);

        let file = dir.join("file.txt");
        fs::write(&file, "one\ntwo\n").expect("write initial file");
        git(dir, &["add", "file.txt"]);
        git(dir, &["commit", "-m", "initial"]);

        fs::write(&file, "one\nthree\n").expect("write modified file");

        let diff = execute_git_diff(
            dir,
            GitDiffParams {
                base: None,
                paths: Some(vec!["file.txt".to_string()]),
            },
        )
        .expect("diff");

        assert!(diff.contains("--- a/file.txt"));
        assert!(diff.contains("+++ b/file.txt"));
        assert!(diff.contains("-two"));
        assert!(diff.contains("+three"));
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

        let blame_json = execute_git_blame(
            dir,
            GitBlameParams {
                file_path: "file.txt".to_string(),
                start_line: Some(1),
                end_line: Some(1),
            },
        )
        .expect("blame");

        let blamed: Vec<BlameLine> = serde_json::from_str(&blame_json).expect("parse blame json");
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

        let show_json = execute_git_show(
            dir,
            GitShowParams {
                rev: Some("HEAD".to_string()),
            },
        )
        .expect("show");

        let shown: ShowEntry = serde_json::from_str(&show_json).expect("parse show json");
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
            },
        )
        .expect("commit");
        assert_eq!(committed["subject"], "initial commit");
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
            },
        )
        .expect_err("empty commit must fail");
        assert!(empty.contains("nothing staged to commit"));
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
            },
        )
        .expect_err("staged gitlink change must fail");
        assert!(error.contains("staged submodule changes are not supported: vendor/reference"));
    }
}
