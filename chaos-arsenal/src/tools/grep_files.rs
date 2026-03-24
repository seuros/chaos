//! MCP tool: grep_files — search file contents via ripgrep.

use std::path::Path;
use std::time::Duration;

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;

use crate::ChaosCtx;
use crate::ChaosServer;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 2000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepFilesParams {
    /// Regex pattern to search for.
    pattern: String,

    /// Optional glob filter for file names (e.g. "*.rs").
    #[serde(default)]
    include: Option<String>,

    /// Directory or file path to search in. Defaults to cwd.
    #[serde(default)]
    path: Option<String>,

    /// Maximum number of matching file paths to return.
    #[serde(default = "default_limit")]
    limit: usize,
}

impl ChaosServer {
    /// Search file contents using ripgrep and return matching file paths.
    #[mcp_tool(name = "grep_files", read_only = true, idempotent = true)]
    async fn grep_files(
        &self,
        _ctx: ChaosCtx<'_>,
        params: Parameters<GrepFilesParams>,
    ) -> ToolResult {
        match execute_params(params.0).await {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }
}

/// Bridge for core's thin adapter — accepts raw JSON arguments.
pub async fn execute(arguments: &serde_json::Value) -> Result<String, String> {
    let params: GrepFilesParams =
        serde_json::from_value(arguments.clone()).map_err(|e| format!("invalid arguments: {e}"))?;
    execute_params(params).await
}

async fn execute_params(params: GrepFilesParams) -> Result<String, String> {
    let pattern = params.pattern.trim();
    if pattern.is_empty() {
        return Err("pattern must not be empty".to_string());
    }

    if params.limit == 0 {
        return Err("limit must be greater than zero".to_string());
    }

    let limit = params.limit.min(MAX_LIMIT);

    let search_path = params.path.as_deref().unwrap_or(".");

    let search_path = Path::new(search_path);
    verify_path_exists(search_path).await?;

    let include = params
        .include
        .as_deref()
        .map(str::trim)
        .and_then(|val| if val.is_empty() { None } else { Some(val) });

    let results = run_rg_search(pattern, include, search_path, limit).await?;

    if results.is_empty() {
        Ok("No matches found.".to_string())
    } else {
        Ok(results.join("\n"))
    }
}

async fn verify_path_exists(path: &Path) -> Result<(), String> {
    tokio::fs::metadata(path)
        .await
        .map_err(|err| format!("unable to access `{}`: {err}", path.display()))?;
    Ok(())
}

pub async fn run_rg_search(
    pattern: &str,
    include: Option<&str>,
    search_path: &Path,
    limit: usize,
) -> Result<Vec<String>, String> {
    let mut command = Command::new("rg");
    command
        .arg("--files-with-matches")
        .arg("--sortr=modified")
        .arg("--regexp")
        .arg(pattern)
        .arg("--no-messages");

    if let Some(glob) = include {
        command.arg("--glob").arg(glob);
    }

    command.arg("--").arg(search_path);

    let output = timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| "rg timed out after 30 seconds".to_string())?
        .map_err(|err| {
            format!("failed to launch rg: {err}. Ensure ripgrep is installed and on PATH.")
        })?;

    match output.status.code() {
        Some(0) => Ok(parse_results(&output.stdout, limit)),
        Some(1) => Ok(Vec::new()),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("rg failed: {stderr}"))
        }
    }
}

pub fn parse_results(stdout: &[u8], limit: usize) -> Vec<String> {
    let mut results = Vec::new();
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(line) {
            if text.is_empty() {
                continue;
            }
            results.push(text.to_string());
            if results.len() == limit {
                break;
            }
        }
    }
    results
}

/// Returns the auto-generated `ToolInfo` for schema extraction by core.
pub fn tool_info() -> mcp_host::prelude::ToolInfo {
    ChaosServer::grep_files_tool_info()
}

pub fn mount(
    router: mcp_host::registry::router::McpToolRouter<ChaosServer>,
) -> mcp_host::registry::router::McpToolRouter<ChaosServer> {
    router.with_tool(
        ChaosServer::grep_files_tool_info(),
        ChaosServer::grep_files_handler,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    #[test]
    fn parses_basic_results() {
        let stdout = b"/tmp/file_a.rs\n/tmp/file_b.rs\n";
        let parsed = parse_results(stdout, 10);
        assert_eq!(
            parsed,
            vec!["/tmp/file_a.rs".to_string(), "/tmp/file_b.rs".to_string()]
        );
    }

    #[test]
    fn parse_truncates_after_limit() {
        let stdout = b"/tmp/file_a.rs\n/tmp/file_b.rs\n/tmp/file_c.rs\n";
        let parsed = parse_results(stdout, 2);
        assert_eq!(
            parsed,
            vec!["/tmp/file_a.rs".to_string(), "/tmp/file_b.rs".to_string()]
        );
    }

    #[tokio::test]
    async fn run_search_returns_results() {
        if !rg_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("match_one.txt"), "alpha beta gamma").unwrap();
        std::fs::write(dir.join("match_two.txt"), "alpha delta").unwrap();
        std::fs::write(dir.join("other.txt"), "omega").unwrap();

        let results = run_rg_search("alpha", None, dir, 10)
            .await
            .expect("search failed");
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|path| path.ends_with("match_one.txt")));
        assert!(results.iter().any(|path| path.ends_with("match_two.txt")));
    }

    #[tokio::test]
    async fn run_search_with_glob_filter() {
        if !rg_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("match_one.rs"), "alpha beta gamma").unwrap();
        std::fs::write(dir.join("match_two.txt"), "alpha delta").unwrap();

        let results = run_rg_search("alpha", Some("*.rs"), dir, 10)
            .await
            .expect("search failed");
        assert_eq!(results.len(), 1);
        assert!(results.iter().all(|path| path.ends_with("match_one.rs")));
    }

    #[tokio::test]
    async fn run_search_respects_limit() {
        if !rg_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("one.txt"), "alpha one").unwrap();
        std::fs::write(dir.join("two.txt"), "alpha two").unwrap();
        std::fs::write(dir.join("three.txt"), "alpha three").unwrap();

        let results = run_rg_search("alpha", None, dir, 2)
            .await
            .expect("search failed");
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn run_search_handles_no_matches() {
        if !rg_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("one.txt"), "omega").unwrap();

        let results = run_rg_search("alpha", None, dir, 5)
            .await
            .expect("search failed");
        assert!(results.is_empty());
    }

    fn rg_available() -> bool {
        StdCommand::new("rg")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}
