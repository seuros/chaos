//! MCP tool: grep_files — search file contents using fff-search.

use std::path::Path;

use fff_search::FFFMode;
use fff_search::FilePicker;
use fff_search::FilePickerOptions;
use fff_search::GrepMode;
use fff_search::GrepSearchOptions;
use fff_search::AiGrepConfig;
use fff_search::QueryParser;
use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::ChaosCtx;
use crate::ChaosServer;
use crate::tools::deserialize_tool_params;
use crate::tools::tool_json_result;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 2000;

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

    /// Maximum number of matching file paths to return.
    #[serde(default = "default_limit")]
    limit: usize,
}

impl ChaosServer {
    /// Search file contents using fff-search and return matching file paths. This does not require an rg binary; use locate_files to fuzzy search file names/paths.
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
    let params: GrepFilesParams = deserialize_tool_params(arguments)?;
    execute_params(params).await
}

pub async fn execute_structured(
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: GrepFilesParams = deserialize_tool_params(arguments)?;
    execute_params_structured(params).await
}

async fn execute_params(params: GrepFilesParams) -> Result<String, String> {
    let structured = execute_params_structured(params).await?;
    let matches = structured
        .get("matches")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if matches.is_empty() {
        Ok("No matches found.".to_string())
    } else {
        Ok(matches
            .into_iter()
            .filter_map(|value| value.as_str().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

async fn execute_params_structured(params: GrepFilesParams) -> Result<serde_json::Value, String> {
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

    // fff-search is sync — run on a blocking thread.
    let pattern = pattern.to_string();
    let search_path = search_path.to_path_buf();
    let include = include.map(String::from);

    let results = tokio::task::spawn_blocking(move || {
        run_grep_search(&pattern, include.as_deref(), &search_path, limit)
    })
    .await
    .map_err(|e| format!("search task failed: {e}"))??;

    let match_count = results.len();
    Ok(serde_json::json!({
        "matches": results,
        "match_count": match_count,
        "limit": limit,
    }))
}

async fn verify_path_exists(path: &Path) -> Result<(), String> {
    tokio::fs::metadata(path)
        .await
        .map_err(|err| format!("unable to access `{}`: {err}", path.display()))?;
    Ok(())
}

/// Search files using fff-search content grep.
///
/// Walks the directory respecting .gitignore, applies an optional glob filter,
/// searches each file for the pattern, collects matching file paths sorted by
/// modification time (newest first), and returns up to `limit` results.
pub fn run_grep_search(
    pattern: &str,
    include: Option<&str>,
    search_path: &Path,
    limit: usize,
) -> Result<Vec<String>, String> {
    regex::bytes::Regex::new(pattern).map_err(|e| format!("invalid regex pattern: {e}"))?;

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: search_path.to_string_lossy().into_owned(),
        mode: FFFMode::Ai,
        watch: false,
        follow_symlinks: false,
        ..Default::default()
    })
    .map_err(|e| format!("failed to create fff picker: {e}"))?;
    picker
        .collect_files()
        .map_err(|e| format!("failed to scan files: {e}"))?;

    let query_text = match include {
        Some(include) => format!("{include} {pattern}"),
        None => pattern.to_string(),
    };
    let parsed = QueryParser::new(AiGrepConfig).parse(&query_text);
    let result = picker.grep(
        &parsed,
        &GrepSearchOptions {
            max_matches_per_file: 1,
            page_limit: limit,
            mode: GrepMode::Regex,
            ..Default::default()
        },
    );

    if let Some(err) = result.regex_fallback_error {
        return Err(format!("invalid regex pattern: {err}"));
    }

    let mut results = result
        .files
        .into_iter()
        .map(|file| {
            let path = file.absolute_path(&picker, picker.base_path());
            (path.to_string_lossy().into_owned(), file.modified)
        })
        .collect::<Vec<_>>();

    // Sort by modification time, newest first.
    results.sort_by_key(|entry| std::cmp::Reverse(entry.1));

    Ok(results
        .into_iter()
        .take(limit)
        .map(|(path, _)| path)
        .collect())
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

    use std::os::unix::fs::symlink;

    #[test]
    fn search_returns_matching_files() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("match_one.txt"), "alpha beta gamma").unwrap();
        std::fs::write(dir.join("match_two.txt"), "alpha delta").unwrap();
        std::fs::write(dir.join("other.txt"), "omega").unwrap();

        let results = run_grep_search("alpha", None, dir, 10).expect("search failed");
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|p| p.ends_with("match_one.txt")));
        assert!(results.iter().any(|p| p.ends_with("match_two.txt")));
    }

    #[test]
    fn search_with_glob_filter() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("match_one.rs"), "alpha beta gamma").unwrap();
        std::fs::write(dir.join("match_two.txt"), "alpha delta").unwrap();

        let results = run_grep_search("alpha", Some("*.rs"), dir, 10).expect("search failed");
        assert_eq!(results.len(), 1);
        assert!(results[0].ends_with("match_one.rs"));
    }

    #[test]
    fn search_respects_limit() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("one.txt"), "alpha one").unwrap();
        std::fs::write(dir.join("two.txt"), "alpha two").unwrap();
        std::fs::write(dir.join("three.txt"), "alpha three").unwrap();

        let results = run_grep_search("alpha", None, dir, 2).expect("search failed");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_handles_no_matches() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("one.txt"), "omega").unwrap();

        let results = run_grep_search("alpha", None, dir, 5).expect("search failed");
        assert!(results.is_empty());
    }

    #[test]
    fn search_rejects_invalid_regex() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let err = run_grep_search("[invalid", None, temp.path(), 10).unwrap_err();
        assert!(err.contains("invalid regex"));
    }

    #[test]
    fn search_matches_non_utf8_files() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(
            dir.join("latin1.txt"),
            [0xff, b'a', b'l', b'p', b'h', b'a', 0xfe],
        )
        .unwrap();

        let results = run_grep_search("alpha", None, dir, 10).expect("search failed");
        assert_eq!(results.len(), 1);
        assert!(results[0].ends_with("latin1.txt"));
    }

    #[test]
    fn search_only_reads_regular_files() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        let real = dir.join("match.txt");
        let alias = dir.join("match-link.txt");
        std::fs::write(&real, "alpha beta gamma").unwrap();
        symlink(&real, &alias).expect("create symlink");

        let results = run_grep_search("alpha", None, dir, 10).expect("search failed");
        assert_eq!(results.len(), 1);
        assert!(results[0].ends_with("match.txt"));
    }

    #[tokio::test]
    async fn rejects_unknown_arguments() {
        let result = execute(&serde_json::json!({
            "pattern": "alpha",
            "pathh": "."
        }))
        .await;

        let err = result.expect_err("unknown field should fail");
        assert!(err.contains("unknown field `pathh`"));
    }
}
