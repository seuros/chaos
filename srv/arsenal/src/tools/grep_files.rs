//! MCP tool: grep_files — local byte-regex content search.

use std::path::Path;

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::ChaosCtx;
use crate::ChaosServer;
use crate::tools::deserialize_tool_params;
use crate::tools::tool_json_result;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 2000;
const MAX_CONTEXT_LINES: usize = 20;
const MAX_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

#[path = "grep_files/content.rs"]
mod content;
#[path = "grep_files/search.rs"]
mod search;

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GrepFilesParams {
    /// Regex pattern to search for.
    pattern: String,

    /// Optional glob filter for file names (e.g. "*.rs").
    #[serde(default)]
    include: Option<String>,

    /// Directory or file path to search in. Defaults to cwd.
    #[serde(default)]
    path: Option<String>,

    /// Max lines (content) or files (files); default 100, cap 2000.
    #[serde(default = "default_limit")]
    limit: usize,

    /// Case-sensitive matching when true, case-insensitive when false.
    /// Omit for smart case (case-insensitive unless the pattern contains uppercase).
    #[serde(default)]
    case_sensitive: Option<bool>,

    /// "content" (default): paths, 1-based line numbers, text. "files": paths only.
    #[serde(default)]
    output_mode: OutputMode,

    /// Context lines per side; content only, default 0, max 20.
    #[serde(default)]
    context_lines: usize,

    /// Lines or files to skip; default 0. Resume with next_offset.
    #[serde(default)]
    offset: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum OutputMode {
    #[default]
    Content,
    Files,
}

impl ChaosServer {
    /// Regex content search: numbered lines, optional context or paths-only output.
    /// Lines cap at 500 UTF-8 bytes (truncated flag); match budget: 128 KiB/page.
    /// If has_more, resume with next_offset. Prefer over grep/rg; use locate_files for names.
    #[mcp_tool(name = "grep_files", read_only = true, open_world = false)]
    async fn grep_files(
        &self,
        _ctx: ChaosCtx<'_>,
        params: Parameters<GrepFilesParams>,
    ) -> ToolResult {
        tool_json_result(execute_params_structured(params.0).await)
    }
}

/// Bridge for core's thin adapter — accepts raw JSON arguments.
pub async fn execute(arguments: &serde_json::Value) -> Result<String, String> {
    execute_structured(arguments)
        .await
        .map(|value| value.to_string())
}

pub async fn execute_structured(
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: GrepFilesParams = deserialize_tool_params(arguments)?;
    execute_params_structured(params).await
}

async fn execute_params_structured(params: GrepFilesParams) -> Result<serde_json::Value, String> {
    if params.pattern.trim().is_empty() {
        return Err("pattern must not be empty".to_string());
    }

    if params.limit == 0 {
        return Err("limit must be greater than zero".to_string());
    }
    if params.context_lines > MAX_CONTEXT_LINES {
        return Err(format!("context_lines must not exceed {MAX_CONTEXT_LINES}"));
    }
    if params.output_mode == OutputMode::Files && params.context_lines != 0 {
        return Err("context_lines requires output_mode \"content\"".to_string());
    }
    // Walking and regex search are synchronous; never block the async executor.
    tokio::task::spawn_blocking(move || search::run_search(&params))
        .await
        .map_err(|e| format!("search task failed: {e}"))?
}

/// Search regular files with a byte regex.
///
/// Walks the directory respecting .gitignore, applies an optional glob filter,
/// searches each file for the pattern, collects matching file paths sorted by
/// modification time (newest first), and returns up to `limit` results.
/// `case_sensitive` overrides smart-case matching when provided.
pub fn run_grep_search(
    pattern: &str,
    include: Option<&str>,
    search_path: &Path,
    limit: usize,
    case_sensitive: Option<bool>,
) -> Result<Vec<String>, String> {
    let result = search::run_search(&GrepFilesParams {
        pattern: pattern.to_string(),
        include: include.map(str::to_string),
        path: Some(search_path.to_string_lossy().into_owned()),
        limit,
        case_sensitive,
        output_mode: OutputMode::Files,
        context_lines: 0,
        offset: 0,
    })?;
    serde_json::from_value(result["matches"].clone())
        .map_err(|e| format!("invalid files-only search result: {e}"))
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
#[path = "grep_files/tests.rs"]
mod tests;
