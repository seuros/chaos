//! MCP tool: locate_files — fuzzy file path search using fff-search.

use std::num::NonZero;
use std::path::Path;
use std::path::PathBuf;

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::ChaosCtx;
use crate::ChaosServer;
use crate::tools::deserialize_tool_params;
use crate::tools::tool_json_result;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 2000;

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

fn default_include_hidden() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct LocateFilesParams {
    /// Fuzzy file path query to search for.
    pattern: String,

    /// Directory to search in. Defaults to cwd.
    #[serde(default)]
    path: Option<String>,

    /// Maximum number of matching file paths to return.
    #[serde(default = "default_limit")]
    limit: usize,

    /// Whether hidden files and directories should be searched.
    #[serde(default = "default_include_hidden")]
    include_hidden: bool,
}

impl ChaosServer {
    /// Fuzzy search file paths using fff-search. Use this to find files by name/path; use grep_files to search file contents.
    #[mcp_tool(name = "locate_files", read_only = true, open_world = false)]
    async fn locate_files(
        &self,
        _ctx: ChaosCtx<'_>,
        params: Parameters<LocateFilesParams>,
    ) -> ToolResult {
        tool_json_result(execute_params_structured(params.0).await)
    }
}

/// Bridge for core's thin adapter — accepts raw JSON arguments.
pub async fn execute(arguments: &serde_json::Value) -> Result<String, String> {
    let params: LocateFilesParams = deserialize_tool_params(arguments)?;
    execute_params(params).await
}

pub async fn execute_structured(
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: LocateFilesParams = deserialize_tool_params(arguments)?;
    execute_params_structured(params).await
}

async fn execute_params(params: LocateFilesParams) -> Result<String, String> {
    let structured = execute_params_structured(params).await?;
    let matches = structured
        .get("matches")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if matches.is_empty() {
        return Ok("No matches found.".to_string());
    }
    let mut lines = matches
        .into_iter()
        .filter_map(|value| value.as_str().map(|path| format_path_for_line(path)))
        .collect::<Vec<_>>();
    let total_match_count = structured
        .get("total_match_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(lines.len() as u64) as usize;
    if total_match_count > lines.len() {
        let hidden_count = total_match_count - lines.len();
        lines.push(format!(
            "... {hidden_count} more matches not shown. Increase limit to see more."
        ));
    }
    Ok(lines.join("\n"))
}

async fn execute_params_structured(params: LocateFilesParams) -> Result<serde_json::Value, String> {
    let pattern = params.pattern.trim();
    if pattern.is_empty() {
        return Err("pattern must not be empty".to_string());
    }

    if params.limit == 0 {
        return Err("limit must be greater than zero".to_string());
    }

    let limit = params.limit.min(MAX_LIMIT);
    let limit = NonZero::new(limit).ok_or_else(|| "limit must be greater than zero".to_string())?;
    let search_path = PathBuf::from(params.path.as_deref().unwrap_or("."));
    verify_search_path(&search_path).await?;

    let pattern = pattern.to_string();
    let include_hidden = params.include_hidden;
    let results = tokio::task::spawn_blocking(move || {
        run_locate_search(&pattern, search_path, limit, include_hidden)
    })
    .await
    .map_err(|e| format!("search task failed: {e}"))??;

    let shown_match_count = results.matches.len();
    Ok(serde_json::json!({
        "matches": results.matches,
        "shown_match_count": shown_match_count,
        "total_match_count": results.total_match_count,
        "truncated": results.total_match_count > shown_match_count,
        "limit": limit.get(),
        "include_hidden": include_hidden,
    }))
}

async fn verify_search_path(path: &Path) -> Result<(), String> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|err| format!("unable to access `{}`: {err}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!("path must be a directory: `{}`", path.display()));
    }
    Ok(())
}

fn format_path_for_line(path: &str) -> String {
    if path.chars().any(char::is_control) {
        format!("{path:?}")
    } else {
        path.to_string()
    }
}

pub fn run_locate_search(
    pattern: &str,
    search_path: PathBuf,
    limit: NonZero<usize>,
    include_hidden: bool,
) -> Result<LocateSearchOutput, String> {
    let results = chaos_locate::run(
        pattern,
        vec![search_path],
        chaos_locate::FileSearchOptions {
            limit,
            include_hidden,
            ..Default::default()
        },
        /*cancel_flag*/ None,
    )
    .map_err(|err| format!("file locate failed: {err}"))?;

    Ok(LocateSearchOutput {
        total_match_count: results.total_match_count,
        matches: results
            .matches
            .into_iter()
            .map(|file_match| file_match.full_path().to_string_lossy().into_owned())
            .collect(),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct LocateSearchOutput {
    pub matches: Vec<String>,
    pub total_match_count: usize,
}

/// Returns the auto-generated `ToolInfo` for schema extraction by core.
pub fn tool_info() -> mcp_host::prelude::ToolInfo {
    ChaosServer::locate_files_tool_info()
}

pub fn mount(
    router: mcp_host::registry::router::McpToolRouter<ChaosServer>,
) -> mcp_host::registry::router::McpToolRouter<ChaosServer> {
    router.with_tool(
        ChaosServer::locate_files_tool_info(),
        ChaosServer::locate_files_handler,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_search_returns_matching_file_paths() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::create_dir_all(dir.join("src/nested")).expect("create dirs");
        std::fs::write(dir.join("src/nested/alpha_widget.rs"), "").expect("write alpha");
        std::fs::write(dir.join("src/beta.rs"), "").expect("write beta");

        let matches = run_locate_search(
            "awrs",
            dir.to_path_buf(),
            NonZero::new(10).expect("non-zero limit"),
            true,
        )
        .expect("locate search");

        assert!(
            matches
                .matches
                .iter()
                .any(|path| path.ends_with("alpha_widget.rs")),
            "matches: {matches:?}"
        );
    }

    #[test]
    fn format_path_for_line_escapes_control_characters() {
        assert_eq!(format_path_for_line("/tmp/normal.md"), "/tmp/normal.md");
        assert_eq!(
            format_path_for_line("/tmp/line\nbreak.md"),
            "\"/tmp/line\\nbreak.md\""
        );
    }

    #[test]
    fn locate_search_includes_hidden_files_by_default_for_tool() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("visible.md"), "").expect("write visible");
        std::fs::write(dir.join(".hidden.md"), "").expect("write hidden");

        let matches = run_locate_search(
            "md",
            dir.to_path_buf(),
            NonZero::new(10).expect("non-zero limit"),
            true,
        )
        .expect("locate search");

        assert!(
            matches
                .matches
                .iter()
                .any(|path| path.ends_with("visible.md")),
            "matches: {matches:?}"
        );
        assert!(
            matches
                .matches
                .iter()
                .any(|path| path.ends_with(".hidden.md")),
            "matches: {matches:?}"
        );
    }

    #[test]
    fn locate_search_can_exclude_hidden_files() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("visible.md"), "").expect("write visible");
        std::fs::write(dir.join(".hidden.md"), "").expect("write hidden");

        let matches = run_locate_search(
            "md",
            dir.to_path_buf(),
            NonZero::new(10).expect("non-zero limit"),
            false,
        )
        .expect("locate search");

        assert!(
            matches
                .matches
                .iter()
                .any(|path| path.ends_with("visible.md")),
            "matches: {matches:?}"
        );
        assert!(
            !matches
                .matches
                .iter()
                .any(|path| path.ends_with(".hidden.md")),
            "matches: {matches:?}"
        );
    }

    #[tokio::test]
    async fn execute_params_reports_truncated_matches() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        std::fs::write(dir.join("one.md"), "").expect("write one");
        std::fs::write(dir.join("two.md"), "").expect("write two");

        let output = execute_params(LocateFilesParams {
            pattern: "md".to_string(),
            path: Some(dir.to_string_lossy().into_owned()),
            limit: 1,
            include_hidden: true,
        })
        .await
        .expect("execute");

        assert!(
            output.contains("more matches not shown"),
            "output: {output}"
        );
    }

    #[tokio::test]
    async fn verify_search_path_rejects_files() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let file = temp.path().join("README.md");
        std::fs::write(&file, "").expect("write file");

        let err = verify_search_path(&file).await.expect_err("file rejected");
        assert!(
            err.contains("path must be a directory"),
            "unexpected error: {err}"
        );
    }
}
