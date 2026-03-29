use std::path::Path;

use gix::bstr::ByteSlice;
use serde::Serialize;

use crate::error::GitError;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub sha: String,
    pub timestamp: i64,
    pub author: String,
    pub subject: String,
}

/// Return commit history.
///
/// - `limit`: max entries (default: 20)
/// - `branch`: ref to walk from (default: HEAD)
pub fn log(
    cwd: &Path,
    limit: Option<usize>,
    branch: Option<&str>,
) -> Result<Vec<LogEntry>, GitError> {
    let repo = open_repo(cwd)?;
    let limit = limit.unwrap_or(20);

    let start = match branch {
        Some(spec) => repo
            .rev_parse_single(spec)
            .map_err(|e| GitError::RefNotFound(format!("{spec}: {e}")))?
            .detach(),
        None => repo
            .head_id()
            .map_err(|e| GitError::Operation(e.to_string()))?
            .detach(),
    };

    let mut entries = Vec::with_capacity(limit);

    let walk = repo
        .rev_walk([start])
        .first_parent_only()
        .all()
        .map_err(|e| GitError::Operation(e.to_string()))?;

    for info in walk.take(limit) {
        let info = info.map_err(|e| GitError::Operation(e.to_string()))?;
        let id = info.id();
        let sha = id.to_string();

        let object = info
            .id()
            .object()
            .map_err(|e| GitError::Operation(e.to_string()))?;

        let commit = object
            .try_into_commit()
            .map_err(|e| GitError::Operation(e.to_string()))?;

        let timestamp = commit
            .time()
            .map_err(|e| GitError::Operation(e.to_string()))?
            .seconds;
        let author = commit
            .author()
            .map_err(|e| GitError::Operation(e.to_string()))?
            .name
            .to_str_lossy()
            .into_owned();
        let subject = commit
            .message()
            .map_err(|e| GitError::Operation(e.to_string()))?
            .title
            .to_str_lossy()
            .into_owned();

        entries.push(LogEntry {
            sha,
            timestamp,
            author,
            subject,
        });
    }

    Ok(entries)
}
