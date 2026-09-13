//! MCP tool: list_dir — recursive directory listing with depth/offset/limit.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs::FileType;
use std::path::Path;
use std::path::PathBuf;

use chaos_wchar::take_bytes_at_char_boundary;
use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::fs;

use crate::ChaosCtx;
use crate::ChaosServer;
use crate::tools::deserialize_tool_params;
use crate::tools::tool_json_result;

const MAX_ENTRY_LENGTH: usize = 500;
const INDENTATION_SPACES: usize = 2;

fn default_offset() -> usize {
    1
}

fn default_limit() -> usize {
    25
}

fn default_depth() -> usize {
    2
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ListDirParams {
    /// Absolute path to the directory to list.
    dir_path: String,

    /// 1-indexed entry offset for pagination.
    #[serde(default = "default_offset")]
    offset: usize,

    /// Maximum number of entries to return.
    #[serde(default = "default_limit")]
    limit: usize,

    /// Maximum directory traversal depth.
    #[serde(default = "default_depth")]
    depth: usize,
}

impl ChaosServer {
    /// List directory contents recursively with configurable depth, offset, and limit.
    #[mcp_tool(name = "list_dir", read_only = true, open_world = false)]
    async fn list_dir(&self, _ctx: ChaosCtx<'_>, params: Parameters<ListDirParams>) -> ToolResult {
        tool_json_result(execute_params_structured(params.0).await)
    }
}

/// Bridge for core's thin adapter — accepts raw JSON arguments.
pub async fn execute(arguments: &serde_json::Value) -> Result<String, String> {
    let params: ListDirParams = deserialize_tool_params(arguments)?;
    execute_params(params).await
}

pub async fn execute_structured(
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: ListDirParams = deserialize_tool_params(arguments)?;
    execute_params_structured(params).await
}

async fn execute_params(params: ListDirParams) -> Result<String, String> {
    let structured = execute_params_structured(params).await?;
    let absolute_path = structured
        .get("absolute_path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let mut output = vec![format!("Absolute path: {absolute_path}")];
    output.extend(
        structured
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.as_str().map(ToString::to_string)),
    );
    if let Some(message) = structured
        .get("truncation_message")
        .and_then(serde_json::Value::as_str)
    {
        output.push(message.to_string());
    }
    Ok(output.join("\n"))
}

async fn execute_params_structured(params: ListDirParams) -> Result<serde_json::Value, String> {
    let ListDirParams {
        dir_path,
        offset,
        limit,
        depth,
    } = params;

    if offset == 0 {
        return Err("offset must be a 1-indexed entry number".to_string());
    }
    if limit == 0 {
        return Err("limit must be greater than zero".to_string());
    }
    if depth == 0 {
        return Err("depth must be greater than zero".to_string());
    }

    let path = PathBuf::from(&dir_path);
    if !path.is_absolute() {
        return Err("dir_path must be an absolute path".to_string());
    }

    let mut entries = list_dir_slice(&path, offset, limit, depth).await?;
    let truncation_message = entries
        .last()
        .filter(|line| line.starts_with("More than "))
        .cloned();
    if truncation_message.is_some() {
        entries.pop();
    }
    Ok(serde_json::json!({
        "absolute_path": path.display().to_string(),
        "entries": entries,
        "offset": offset,
        "limit": limit,
        "depth": depth,
        "truncated": truncation_message.is_some(),
        "truncation_message": truncation_message,
    }))
}

pub async fn list_dir_slice(
    path: &Path,
    offset: usize,
    limit: usize,
    depth: usize,
) -> Result<Vec<String>, String> {
    let mut entries = Vec::new();
    collect_entries(path, Path::new(""), depth, &mut entries).await?;

    if entries.is_empty() {
        return Ok(Vec::new());
    }

    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));

    let start_index = offset - 1;
    if start_index >= entries.len() {
        return Err("offset exceeds directory entry count".to_string());
    }

    let remaining_entries = entries.len() - start_index;
    let capped_limit = limit.min(remaining_entries);
    let end_index = start_index + capped_limit;
    let selected_entries = &entries[start_index..end_index];
    let mut formatted = Vec::with_capacity(selected_entries.len());

    for entry in selected_entries {
        formatted.push(format_entry_line(entry));
    }

    if end_index < entries.len() {
        formatted.push(format!("More than {capped_limit} entries found"));
    }

    Ok(formatted)
}

async fn collect_entries(
    dir_path: &Path,
    relative_prefix: &Path,
    depth: usize,
    entries: &mut Vec<DirEntry>,
) -> Result<(), String> {
    let mut queue = VecDeque::new();
    queue.push_back((dir_path.to_path_buf(), relative_prefix.to_path_buf(), depth));

    while let Some((current_dir, prefix, remaining_depth)) = queue.pop_front() {
        let mut read_dir = fs::read_dir(&current_dir)
            .await
            .map_err(|err| format!("failed to read directory: {err}"))?;

        let mut dir_entries = Vec::new();

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|err| format!("failed to read directory: {err}"))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|err| format!("failed to inspect entry: {err}"))?;

            let file_name = entry.file_name();
            let relative_path = if prefix.as_os_str().is_empty() {
                PathBuf::from(&file_name)
            } else {
                prefix.join(&file_name)
            };

            let display_name = format_entry_component(&file_name);
            let display_depth = prefix.components().count();
            let sort_key = format_entry_name(&relative_path);
            let kind = DirEntryKind::from(&file_type);
            dir_entries.push((
                entry.path(),
                relative_path,
                kind,
                DirEntry {
                    name: sort_key,
                    display_name,
                    depth: display_depth,
                    kind,
                },
            ));
        }

        dir_entries.sort_unstable_by(|a, b| a.3.name.cmp(&b.3.name));

        for (entry_path, relative_path, kind, dir_entry) in dir_entries {
            if kind == DirEntryKind::Directory && remaining_depth > 1 {
                queue.push_back((entry_path, relative_path, remaining_depth - 1));
            }
            entries.push(dir_entry);
        }
    }

    Ok(())
}

fn format_entry_name(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace("\\", "/");
    if normalized.len() > MAX_ENTRY_LENGTH {
        take_bytes_at_char_boundary(&normalized, MAX_ENTRY_LENGTH).to_string()
    } else {
        normalized
    }
}

fn format_entry_component(name: &OsStr) -> String {
    let normalized = name.to_string_lossy();
    if normalized.len() > MAX_ENTRY_LENGTH {
        take_bytes_at_char_boundary(&normalized, MAX_ENTRY_LENGTH).to_string()
    } else {
        normalized.to_string()
    }
}

fn format_entry_line(entry: &DirEntry) -> String {
    let indent = " ".repeat(entry.depth * INDENTATION_SPACES);
    let mut name = entry.display_name.clone();
    match entry.kind {
        DirEntryKind::Directory => name.push('/'),
        DirEntryKind::Symlink => name.push('@'),
        DirEntryKind::Other => name.push('?'),
        DirEntryKind::File => {}
    }
    format!("{indent}{name}")
}

#[derive(Clone)]
struct DirEntry {
    name: String,
    display_name: String,
    depth: usize,
    kind: DirEntryKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

impl From<&FileType> for DirEntryKind {
    fn from(file_type: &FileType) -> Self {
        if file_type.is_symlink() {
            DirEntryKind::Symlink
        } else if file_type.is_dir() {
            DirEntryKind::Directory
        } else if file_type.is_file() {
            DirEntryKind::File
        } else {
            DirEntryKind::Other
        }
    }
}

/// Returns the auto-generated `ToolInfo` for schema extraction by core.
pub fn tool_info() -> mcp_host::prelude::ToolInfo {
    ChaosServer::list_dir_tool_info()
}

pub fn mount(
    router: mcp_host::registry::router::McpToolRouter<ChaosServer>,
) -> mcp_host::registry::router::McpToolRouter<ChaosServer> {
    router.with_tool(
        ChaosServer::list_dir_tool_info(),
        ChaosServer::list_dir_handler,
        None,
    )
}

#[cfg(test)]
#[path = "list_dir/tests.rs"]
mod tests;
