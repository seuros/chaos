use std::path::Path;

use serde::Deserialize;
use serde::Serialize;

use crate::error::GitError;
use crate::ext::GitResultExt;
use crate::ext::RepoRelativePathMessages;
use crate::ext::normalize_repo_relative_path;
use crate::open_repo;

const MAX_BLOB_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAtRev {
    pub path: String,
    pub rev: String,
    pub sha: String,
    pub blob_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    pub total_lines: usize,
    pub content: String,
}

/// Show a tracked file at a revision.
///
/// - `file_path`: path relative to the repository root
/// - `rev`: revision to resolve (default: HEAD)
/// - `lines`: optional `(start, end)` 1-indexed inclusive range
pub fn show_file(
    cwd: &Path,
    file_path: &str,
    rev: Option<&str>,
    lines: Option<(usize, usize)>,
) -> Result<FileAtRev, GitError> {
    let path = normalize_repo_relative_path(
        file_path,
        RepoRelativePathMessages {
            empty: "file_path must not be empty",
            nul: "file_path must not contain NUL bytes",
            root: "repository root cannot be shown as a file; pass a file path",
            git_dir: "paths inside the Git directory cannot be shown",
        },
    )?;
    if let Some((start, end)) = lines {
        validate_line_range(start, end)?;
    }

    let repo = open_repo(cwd)?;
    let spec = rev.unwrap_or("HEAD");
    let spec_id = repo
        .rev_parse_single(spec)
        .map_err(|error| GitError::RefNotFound(format!("{spec}: {error}")))?
        .detach();

    let object = repo.find_object(spec_id).git_op()?;
    let (sha, tree) = match object.peel_to_commit() {
        Ok(commit) => {
            let sha = commit.id().to_string();
            let tree = commit.tree().git_op()?;
            (sha, tree)
        }
        Err(_) => {
            let object = repo.find_object(spec_id).git_op()?;
            let tree = object.peel_to_tree().git_op()?;
            (spec_id.to_string(), tree)
        }
    };

    let entry = tree
        .lookup_entry_by_path(path.as_str())
        .git_op()?
        .ok_or_else(|| GitError::PathNotFound(path.clone()))?;
    let mode = entry.mode();
    if mode.is_tree() {
        return Err(GitError::Unsupported(format!(
            "path is a directory: {path}"
        )));
    }
    if mode.is_commit() {
        return Err(GitError::Unsupported(format!(
            "submodules are not supported: {path}"
        )));
    }
    if !mode.is_blob_or_symlink() {
        return Err(GitError::Unsupported(format!(
            "unsupported tree entry type {}: {path}",
            mode.as_str()
        )));
    }

    let blob_id = entry.object_id();
    let header = repo.find_header(blob_id).git_op()?;
    if header.size() > MAX_BLOB_BYTES {
        return Err(GitError::Unsupported(format!(
            "{path} is {} bytes (limit {MAX_BLOB_BYTES})",
            header.size()
        )));
    }

    let blob = repo.find_object(blob_id).git_op()?;
    if is_binary(&blob.data) {
        return Err(GitError::Unsupported(format!("binary file: {path}")));
    }
    let text = std::str::from_utf8(&blob.data)
        .map_err(|_| GitError::Unsupported(format!("binary file: {path}")))?;

    let total_lines = line_count(text);
    let (start_line, end_line, content) = match lines {
        None => (None, None, text.to_string()),
        Some((start, end)) => {
            if total_lines == 0 || start > total_lines {
                return Err(GitError::InvalidInput(format!(
                    "start_line {start} is past end of file ({total_lines} lines)"
                )));
            }
            let end = end.min(total_lines);
            (Some(start), Some(end), slice_lines(text, start, end))
        }
    };

    Ok(FileAtRev {
        path,
        rev: spec.to_string(),
        sha,
        blob_sha: blob_id.to_string(),
        start_line,
        end_line,
        total_lines,
        content,
    })
}

fn validate_line_range(start: usize, end: usize) -> Result<(), GitError> {
    if start == 0 || end == 0 {
        return Err(GitError::InvalidInput(
            "start_line and end_line must be 1-indexed".to_string(),
        ));
    }
    if end < start {
        return Err(GitError::InvalidInput(
            "end_line must be greater than or equal to start_line".to_string(),
        ));
    }
    Ok(())
}

fn line_count(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.lines().count()
    }
}

fn slice_lines(content: &str, start: usize, end: usize) -> String {
    let mut start_byte = None;
    let mut end_byte = content.len();
    let mut idx = 0usize;
    for (line_no, line) in (1usize..).zip(content.split_inclusive('\n')) {
        if line_no == start {
            start_byte = Some(idx);
        }
        idx += line.len();
        if line_no == end {
            end_byte = idx;
            break;
        }
    }
    content[start_byte.unwrap_or(content.len())..end_byte].to_string()
}

fn is_binary(content: &[u8]) -> bool {
    content[..content.len().min(8192)].contains(&0)
}
