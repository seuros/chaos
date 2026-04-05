use std::path::Path;

use gix::bstr::ByteSlice;
use serde::Deserialize;
use serde::Serialize;

use crate::error::GitError;
use crate::ext::GitResultExt;
use crate::open_repo;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

    let head = repo.head_id().git_op()?;

    let head_obj = head.object().git_op()?;
    let head_commit = head_obj.try_into_commit().git_op()?;
    let author = head_commit.author().git_op()?.name.to_str_lossy().into_owned();
    let commit = head_commit.tree().git_op()?;

    let entry = commit
        .lookup_entry_by_path(file_path)
        .git_op()?
        .ok_or_else(|| GitError::PathNotFound(file_path.to_string()))?;

    let blob = entry.object().git_op()?;

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
        if let Some((start, end)) = lines
            && (line_no < start || line_no > end)
        {
            continue;
        }
        result.push(BlameLine {
            sha: head_sha_short.to_string(),
            author: author.clone(), // Placeholder until full blame traversal exists.
            line_no,
            content: line.to_string(),
        });
    }

    Ok(result)
}
