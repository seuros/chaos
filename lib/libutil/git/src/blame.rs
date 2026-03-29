use std::path::Path;

use serde::Serialize;

use crate::error::GitError;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct BlameLine {
    pub sha: String,
    pub author: String,
    pub line_no: usize,
    pub content: String,
}

/// Blame a file, optionally restricting to a line range.
///
/// - `path`: file path relative to repo root
/// - `lines`: optional `(start, end)` 1-indexed inclusive range
pub fn blame(
    cwd: &Path,
    file_path: &str,
    lines: Option<(usize, usize)>,
) -> Result<Vec<BlameLine>, GitError> {
    let repo = open_repo(cwd)?;

    let head = repo
        .head_id()
        .map_err(|e| GitError::Operation(e.to_string()))?;

    let commit = head
        .object()
        .map_err(|e| GitError::Operation(e.to_string()))?
        .peel_to_tree()
        .map_err(|e| GitError::Operation(e.to_string()))?;

    let entry = commit
        .lookup_entry_by_path(file_path)
        .map_err(|e| GitError::Operation(e.to_string()))?
        .ok_or_else(|| GitError::PathNotFound(file_path.to_string()))?;

    let blob = entry
        .object()
        .map_err(|e| GitError::Operation(e.to_string()))?;

    let content = std::str::from_utf8(&blob.data)
        .map_err(|e| GitError::Operation(format!("binary file: {e}")))?;

    // Without full blame traversal (which gix doesn't yet expose as a
    // simple API), we return the file content attributed to HEAD.
    // This is a placeholder until gix gains a blame API.
    let mut result = Vec::new();
    let head_sha = head.to_string();
    let head_sha_short = &head_sha[..8.min(head_sha.len())];

    for (i, line) in content.lines().enumerate() {
        let line_no = i + 1;
        if let Some((start, end)) = lines {
            if line_no < start || line_no > end {
                continue;
            }
        }
        result.push(BlameLine {
            sha: head_sha_short.to_string(),
            author: String::new(), // TODO: full blame traversal
            line_no,
            content: line.to_string(),
        });
    }

    Ok(result)
}
