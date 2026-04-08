//! MCP tool: grep_files — search file contents using ripgrep libraries.

use std::path::Path;
use std::sync::Mutex;

use grep_regex::RegexMatcher;
use grep_searcher::Searcher;
use grep_searcher::sinks::Bytes;
use ignore::WalkBuilder;
use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::ChaosCtx;
use crate::ChaosServer;

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
    /// Search file contents using ripgrep libraries and return matching file paths.
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

    // grep-searcher is sync — run on a blocking thread.
    let pattern = pattern.to_string();
    let search_path = search_path.to_path_buf();
    let include = include.map(String::from);

    let results = tokio::task::spawn_blocking(move || {
        run_grep_search(&pattern, include.as_deref(), &search_path, limit)
    })
    .await
    .map_err(|e| format!("search task failed: {e}"))??;

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

/// Search files using grep-searcher + ignore crate (same engine as ripgrep).
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
    let matcher = RegexMatcher::new(pattern).map_err(|e| format!("invalid regex pattern: {e}"))?;

    let mut walk_builder = WalkBuilder::new(search_path);
    walk_builder.hidden(true).git_ignore(true).git_global(true);

    if let Some(glob) = include {
        // Override the default type set with a custom glob.
        let mut types_builder = ignore::types::TypesBuilder::new();
        types_builder
            .add("custom", glob)
            .map_err(|e| format!("invalid glob pattern: {e}"))?;
        types_builder.select("custom");
        walk_builder.types(
            types_builder
                .build()
                .map_err(|e| format!("failed to build glob filter: {e}"))?,
        );
    }

    // Collect matching files with their modification times for sorting.
    let matches: Mutex<Vec<(String, std::time::SystemTime)>> = Mutex::new(Vec::new());
    let first_error: Mutex<Option<String>> = Mutex::new(None);

    walk_builder.build_parallel().run(|| {
        let matcher = matcher.clone();
        let matches = &matches;
        let first_error = &first_error;
        Box::new(move |entry| {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    record_first_error(first_error, format!("search walk failed: {err}"));
                    return ignore::WalkState::Quit;
                }
            };

            // Skip anything that isn't a regular file — directories, FIFOs,
            // sockets, and device nodes would block or make no sense to search.
            let is_regular = entry.file_type().is_some_and(|ft| ft.is_file());
            if !is_regular {
                return ignore::WalkState::Continue;
            }

            // Search the file for at least one match. Use a byte-oriented
            // sink so non-UTF-8 files (Latin-1, generated assets) still match.
            let mut found = false;
            let mut searcher = Searcher::new();
            if let Err(err) = searcher.search_path(
                &matcher,
                entry.path(),
                Bytes(|_line_num, _line| {
                    found = true;
                    // Stop after first match — we only need to know the file matches.
                    Ok(false)
                }),
            ) {
                record_first_error(
                    first_error,
                    format!("failed to search `{}`: {err}", entry.path().display()),
                );
                return ignore::WalkState::Quit;
            }

            if found {
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

                match matches.lock() {
                    Ok(mut guard) => {
                        guard.push((entry.path().to_string_lossy().to_string(), mtime));
                    }
                    Err(err) => {
                        record_first_error(first_error, format!("lock error: {err}"));
                        return ignore::WalkState::Quit;
                    }
                }
            }

            ignore::WalkState::Continue
        })
    });

    if let Some(err) = first_error
        .into_inner()
        .map_err(|e| format!("lock error: {e}"))?
    {
        return Err(err);
    }

    let mut results = matches
        .into_inner()
        .map_err(|e| format!("lock error: {e}"))?;

    // Sort by modification time, newest first (matching rg --sortr=modified).
    results.sort_by(|a, b| b.1.cmp(&a.1));

    Ok(results
        .into_iter()
        .take(limit)
        .map(|(path, _)| path)
        .collect())
}

fn record_first_error(slot: &Mutex<Option<String>>, message: String) {
    if let Ok(mut guard) = slot.lock() {
        guard.get_or_insert(message);
    }
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

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
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
            dir.join("latin1.bin"),
            [0xff, b'a', b'l', b'p', b'h', b'a', 0xfe],
        )
        .unwrap();

        let results = run_grep_search("alpha", None, dir, 10).expect("search failed");
        assert_eq!(results.len(), 1);
        assert!(results[0].ends_with("latin1.bin"));
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn search_reports_unreadable_file_errors() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let dir = temp.path();
        let unreadable = dir.join("secret.txt");
        std::fs::write(&unreadable, "alpha hidden").unwrap();

        let mut perms = std::fs::metadata(&unreadable).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&unreadable, perms).unwrap();

        let err = run_grep_search("alpha", None, dir, 10).unwrap_err();

        let mut restore = std::fs::metadata(&unreadable).unwrap().permissions();
        restore.set_mode(0o644);
        std::fs::set_permissions(&unreadable, restore).unwrap();

        assert!(err.contains("secret.txt"));
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
