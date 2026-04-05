use std::path::Path;

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::GitCtx;
use crate::GitServer;

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

impl GitServer {
    #[mcp_tool(name = "git_diff", read_only = true, idempotent = true)]
    async fn git_diff(&self, _ctx: GitCtx<'_>, params: Parameters<GitDiffParams>) -> ToolResult {
        match execute_git_diff(Path::new("."), params.0) {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }

    #[mcp_tool(name = "git_log", read_only = true, idempotent = true)]
    async fn git_log(&self, _ctx: GitCtx<'_>, params: Parameters<GitLogParams>) -> ToolResult {
        match execute_git_log(Path::new("."), params.0) {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }

    #[mcp_tool(name = "git_show", read_only = true, idempotent = true)]
    async fn git_show(&self, _ctx: GitCtx<'_>, params: Parameters<GitShowParams>) -> ToolResult {
        match execute_git_show(Path::new("."), params.0) {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }

    #[mcp_tool(name = "git_blame", read_only = true, idempotent = true)]
    async fn git_blame(&self, _ctx: GitCtx<'_>, params: Parameters<GitBlameParams>) -> ToolResult {
        match execute_git_blame(Path::new("."), params.0) {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }

    #[mcp_tool(name = "git_repo", read_only = true, idempotent = true)]
    async fn git_repo(&self, _ctx: GitCtx<'_>, params: Parameters<GitRepoParams>) -> ToolResult {
        match execute_git_repo(Path::new("."), params.0) {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }

    #[mcp_tool(name = "git_status", read_only = true, idempotent = true)]
    async fn git_status(
        &self,
        _ctx: GitCtx<'_>,
        params: Parameters<GitStatusParams>,
    ) -> ToolResult {
        match execute_git_status(Path::new("."), params.0) {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }

    #[mcp_tool(name = "git_branches", read_only = true, idempotent = true)]
    async fn git_branches(
        &self,
        _ctx: GitCtx<'_>,
        params: Parameters<GitBranchesParams>,
    ) -> ToolResult {
        match execute_git_branches(Path::new("."), params.0) {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }

    #[mcp_tool(name = "git_remotes", read_only = true, idempotent = true)]
    async fn git_remotes(
        &self,
        _ctx: GitCtx<'_>,
        params: Parameters<GitRemotesParams>,
    ) -> ToolResult {
        match execute_git_remotes(Path::new("."), params.0) {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
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
    ]
}

pub fn execute_git_diff(cwd: &Path, params: GitDiffParams) -> Result<String, String> {
    let GitDiffParams { base, paths } = params;
    let path_refs = paths
        .as_ref()
        .map(|items| items.iter().map(String::as_str).collect::<Vec<_>>());
    crate::diff(cwd, base.as_deref(), path_refs.as_deref()).map_err(|e| e.to_string())
}

pub fn execute_git_log(cwd: &Path, params: GitLogParams) -> Result<String, String> {
    let entries = crate::log(cwd, Some(params.limit), params.branch.as_deref())
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
}

pub fn execute_git_show(cwd: &Path, params: GitShowParams) -> Result<String, String> {
    let entry = crate::show(cwd, params.rev.as_deref()).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())
}

pub fn execute_git_blame(cwd: &Path, params: GitBlameParams) -> Result<String, String> {
    let lines = match (params.start_line, params.end_line) {
        (Some(start), Some(end)) => Some((start, end)),
        (None, None) => None,
        _ => {
            return Err(
                "start_line and end_line must either both be provided or both be omitted"
                    .to_string(),
            )
        }
    };
    let blamed = crate::blame(cwd, &params.file_path, lines).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&blamed).map_err(|e| e.to_string())
}

pub fn execute_git_repo(cwd: &Path, _params: GitRepoParams) -> Result<String, String> {
    let info = crate::repo_info(cwd).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
}

pub fn execute_git_status(cwd: &Path, _params: GitStatusParams) -> Result<String, String> {
    let info = crate::status(cwd).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
}

pub fn execute_git_branches(cwd: &Path, _params: GitBranchesParams) -> Result<String, String> {
    let info = crate::branches(cwd).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
}

pub fn execute_git_remotes(cwd: &Path, _params: GitRemotesParams) -> Result<String, String> {
    let info = crate::remotes(cwd).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::GitBlameParams;
    use super::GitDiffParams;
    use super::GitShowParams;
    use super::execute_git_blame;
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
        assert!(status.success(), "git command failed: git {}", args.join(" "));
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
}
