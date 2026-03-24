use std::collections::VecDeque;
use std::path::PathBuf;

use codex_utils_string::take_bytes_at_char_boundary;
use mcp_host::prelude::*;
use mcp_host::registry::router::McpToolRouter;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{ChaosCtx, ChaosServer};

pub const MAX_LINE_LENGTH: usize = 500;
const TAB_WIDTH: usize = 4;

// TODO(jif) add support for block comments
const COMMENT_PREFIXES: &[&str] = &["#", "//", "--"];

/// Parameters accepted by the `read_file` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadFileParams {
    /// Absolute path to the file.
    file_path: String,

    /// 1-indexed line number to start reading from (default: 1).
    #[serde(default = "defaults::offset")]
    offset: usize,

    /// Maximum number of lines to return (default: 2000).
    #[serde(default = "defaults::limit")]
    limit: usize,

    /// Mode selector: "slice" for simple ranges (default) or "indentation"
    /// to expand around an anchor line.
    #[serde(default)]
    mode: ReadMode,

    /// Optional indentation configuration used when mode is "indentation".
    #[serde(default)]
    indentation: Option<IndentationParams>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ReadMode {
    #[default]
    Slice,
    Indentation,
}

/// Additional configuration for indentation-aware reads.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct IndentationParams {
    /// Anchor line to center the indentation lookup on (defaults to offset).
    #[serde(default)]
    pub anchor_line: Option<usize>,

    /// How many parent indentation levels (smaller indents) to include. 0 means unlimited.
    #[serde(default = "defaults::max_levels")]
    pub max_levels: usize,

    /// When true, include additional blocks that share the anchor indentation.
    #[serde(default = "defaults::include_siblings")]
    pub include_siblings: bool,

    /// Include doc comments or attributes directly above the selected block.
    #[serde(default = "defaults::include_header")]
    pub include_header: bool,

    /// Hard cap on the number of lines returned when using indentation mode.
    #[serde(default)]
    pub max_lines: Option<usize>,
}

impl Default for IndentationParams {
    fn default() -> Self {
        Self {
            anchor_line: None,
            max_levels: defaults::max_levels(),
            include_siblings: defaults::include_siblings(),
            include_header: defaults::include_header(),
            max_lines: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

impl ChaosServer {
    /// Reads a local file with 1-indexed line numbers, supporting slice and
    /// indentation-aware block modes.
    #[mcp_tool(name = "read_file", read_only = true, idempotent = true)]
    async fn read_file(
        &self,
        _ctx: ChaosCtx<'_>,
        params: Parameters<ReadFileParams>,
    ) -> ToolResult {
        match execute_params(params.0).await {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }
}

/// Standalone execution entry point — callable from both MCP and core adapter.
///
/// Takes raw JSON arguments, returns the formatted file content or an error message.
pub async fn execute(arguments: &serde_json::Value) -> Result<String, String> {
    let params: ReadFileParams =
        serde_json::from_value(arguments.clone()).map_err(|e| format!("invalid arguments: {e}"))?;
    execute_params(params).await
}

/// Shared logic for both MCP handler and JSON adapter paths.
async fn execute_params(params: ReadFileParams) -> Result<String, String> {
    let ReadFileParams {
        file_path,
        offset,
        limit,
        mode,
        indentation,
    } = params;

    if offset == 0 {
        return Err("offset must be a 1-indexed line number".to_string());
    }
    if limit == 0 {
        return Err("limit must be greater than zero".to_string());
    }

    let path = PathBuf::from(&file_path);
    if !path.is_absolute() {
        return Err("file_path must be an absolute path".to_string());
    }

    let collected = match mode {
        ReadMode::Slice => slice::read(&path, offset, limit).await?,
        ReadMode::Indentation => {
            let opts = indentation.unwrap_or_default();
            indentation_mode::read_block(&path, offset, limit, opts).await?
        }
    };

    Ok(collected.join("\n"))
}

/// Returns the auto-generated `ToolInfo` for schema extraction by core.
pub fn tool_info() -> mcp_host::prelude::ToolInfo {
    ChaosServer::read_file_tool_info()
}

pub fn mount(router: McpToolRouter<ChaosServer>) -> McpToolRouter<ChaosServer> {
    router.with_tool(
        ChaosServer::read_file_tool_info(),
        ChaosServer::read_file_handler,
        None,
    )
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct LineRecord {
    number: usize,
    raw: String,
    display: String,
    indent: usize,
}

impl LineRecord {
    fn trimmed(&self) -> &str {
        self.raw.trim_start()
    }

    fn is_blank(&self) -> bool {
        self.trimmed().is_empty()
    }

    fn is_comment(&self) -> bool {
        COMMENT_PREFIXES
            .iter()
            .any(|prefix| self.raw.trim().starts_with(prefix))
    }
}

fn format_line(bytes: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(bytes);
    if decoded.len() > MAX_LINE_LENGTH {
        take_bytes_at_char_boundary(&decoded, MAX_LINE_LENGTH).to_string()
    } else {
        decoded.into_owned()
    }
}

fn trim_empty_lines(out: &mut VecDeque<&LineRecord>) {
    while matches!(out.front(), Some(line) if line.raw.trim().is_empty()) {
        out.pop_front();
    }
    while matches!(out.back(), Some(line) if line.raw.trim().is_empty()) {
        out.pop_back();
    }
}

// ---------------------------------------------------------------------------
// Slice mode
// ---------------------------------------------------------------------------

pub mod slice {
    use super::format_line;
    use std::path::Path;
    use tokio::fs::File;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::BufReader;

    pub async fn read(path: &Path, offset: usize, limit: usize) -> Result<Vec<String>, String> {
        let file = File::open(path)
            .await
            .map_err(|err| format!("failed to read file: {err}"))?;

        let mut reader = BufReader::new(file);
        let mut collected = Vec::new();
        let mut seen = 0usize;
        let mut buffer = Vec::new();

        loop {
            buffer.clear();
            let bytes_read = reader
                .read_until(b'\n', &mut buffer)
                .await
                .map_err(|err| format!("failed to read file: {err}"))?;

            if bytes_read == 0 {
                break;
            }

            if buffer.last() == Some(&b'\n') {
                buffer.pop();
                if buffer.last() == Some(&b'\r') {
                    buffer.pop();
                }
            }

            seen += 1;

            if seen < offset {
                continue;
            }

            if collected.len() == limit {
                break;
            }

            let formatted = format_line(&buffer);
            collected.push(format!("L{seen}: {formatted}"));

            if collected.len() == limit {
                break;
            }
        }

        if seen < offset {
            return Err("offset exceeds file length".to_string());
        }

        Ok(collected)
    }
}

// ---------------------------------------------------------------------------
// Indentation mode
// ---------------------------------------------------------------------------

pub mod indentation_mode {
    use super::{IndentationParams, LineRecord, TAB_WIDTH, format_line, trim_empty_lines};
    use std::collections::VecDeque;
    use std::path::Path;
    use tokio::fs::File;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::BufReader;

    pub async fn read_block(
        path: &Path,
        offset: usize,
        limit: usize,
        options: IndentationParams,
    ) -> Result<Vec<String>, String> {
        let anchor_line = options.anchor_line.unwrap_or(offset);
        if anchor_line == 0 {
            return Err("anchor_line must be a 1-indexed line number".to_string());
        }

        let guard_limit = options.max_lines.unwrap_or(limit);
        if guard_limit == 0 {
            return Err("max_lines must be greater than zero".to_string());
        }

        let collected = collect_file_lines(path).await?;
        if collected.is_empty() || anchor_line > collected.len() {
            return Err("anchor_line exceeds file length".to_string());
        }

        let anchor_index = anchor_line - 1;
        let effective_indents = compute_effective_indents(&collected);
        let anchor_indent = effective_indents[anchor_index];

        let min_indent = if options.max_levels == 0 {
            0
        } else {
            anchor_indent.saturating_sub(options.max_levels * TAB_WIDTH)
        };

        let final_limit = limit.min(guard_limit).min(collected.len());

        if final_limit == 1 {
            return Ok(vec![format!(
                "L{}: {}",
                collected[anchor_index].number, collected[anchor_index].display
            )]);
        }

        let mut i: isize = anchor_index as isize - 1;
        let mut j: usize = anchor_index + 1;
        let mut i_counter_min_indent = 0;
        let mut j_counter_min_indent = 0;

        let mut out = VecDeque::with_capacity(limit);
        out.push_back(&collected[anchor_index]);

        while out.len() < final_limit {
            let mut progressed = 0;

            // Up.
            if i >= 0 {
                let iu = i as usize;
                if effective_indents[iu] >= min_indent {
                    out.push_front(&collected[iu]);
                    progressed += 1;
                    i -= 1;

                    if effective_indents[iu] == min_indent && !options.include_siblings {
                        let allow_header_comment =
                            options.include_header && collected[iu].is_comment();
                        let can_take_line = allow_header_comment || i_counter_min_indent == 0;

                        if can_take_line {
                            i_counter_min_indent += 1;
                        } else {
                            out.pop_front();
                            progressed -= 1;
                            i = -1;
                        }
                    }

                    if out.len() >= final_limit {
                        break;
                    }
                } else {
                    i = -1;
                }
            }

            // Down.
            if j < collected.len() {
                let ju = j;
                if effective_indents[ju] >= min_indent {
                    out.push_back(&collected[ju]);
                    progressed += 1;
                    j += 1;

                    if effective_indents[ju] == min_indent && !options.include_siblings {
                        if j_counter_min_indent > 0 {
                            out.pop_back();
                            progressed -= 1;
                            j = collected.len();
                        }
                        j_counter_min_indent += 1;
                    }
                } else {
                    j = collected.len();
                }
            }

            if progressed == 0 {
                break;
            }
        }

        trim_empty_lines(&mut out);

        Ok(out
            .into_iter()
            .map(|record| format!("L{}: {}", record.number, record.display))
            .collect())
    }

    async fn collect_file_lines(path: &Path) -> Result<Vec<LineRecord>, String> {
        let file = File::open(path)
            .await
            .map_err(|err| format!("failed to read file: {err}"))?;

        let mut reader = BufReader::new(file);
        let mut buffer = Vec::new();
        let mut lines = Vec::new();
        let mut number = 0usize;

        loop {
            buffer.clear();
            let bytes_read = reader
                .read_until(b'\n', &mut buffer)
                .await
                .map_err(|err| format!("failed to read file: {err}"))?;

            if bytes_read == 0 {
                break;
            }

            if buffer.last() == Some(&b'\n') {
                buffer.pop();
                if buffer.last() == Some(&b'\r') {
                    buffer.pop();
                }
            }

            number += 1;
            let raw = String::from_utf8_lossy(&buffer).into_owned();
            let indent = measure_indent(&raw);
            let display = format_line(&buffer);
            lines.push(LineRecord {
                number,
                raw,
                display,
                indent,
            });
        }

        Ok(lines)
    }

    fn compute_effective_indents(records: &[LineRecord]) -> Vec<usize> {
        let mut effective = Vec::with_capacity(records.len());
        let mut previous_indent = 0usize;
        for record in records {
            if record.is_blank() {
                effective.push(previous_indent);
            } else {
                previous_indent = record.indent;
                effective.push(previous_indent);
            }
        }
        effective
    }

    fn measure_indent(line: &str) -> usize {
        line.chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .map(|c| if c == '\t' { TAB_WIDTH } else { 1 })
            .sum()
    }
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

mod defaults {
    pub fn offset() -> usize {
        1
    }

    pub fn limit() -> usize {
        2000
    }

    pub fn max_levels() -> usize {
        0
    }

    pub fn include_siblings() -> bool {
        false
    }

    pub fn include_header() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_generates_valid_json() {
        let info = ChaosServer::read_file_tool_info();
        assert_eq!(info.name, "read_file");
        assert!(info.description.is_some());

        // Verify input_schema is a valid JSON object with properties
        let schema = &info.input_schema;
        assert!(schema.is_object(), "input_schema must be a JSON object");

        let obj = schema.as_object().unwrap();
        let props = obj.get("properties").unwrap().as_object().unwrap();
        assert!(
            props.contains_key("file_path"),
            "schema must have file_path property"
        );
        assert!(
            props.contains_key("offset"),
            "schema must have offset property"
        );
        assert!(
            props.contains_key("limit"),
            "schema must have limit property"
        );
        assert!(props.contains_key("mode"), "schema must have mode property");
        assert!(
            props.contains_key("indentation"),
            "schema must have indentation property"
        );

        // Verify required fields
        let required = obj.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("file_path".to_string())));
    }

    #[test]
    fn router_contains_all_tools() {
        let router = crate::tools::router();
        let tools = router.list();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"grep_files"));
        assert!(names.contains(&"list_dir"));
        assert_eq!(tools.len(), 3);
    }
}
