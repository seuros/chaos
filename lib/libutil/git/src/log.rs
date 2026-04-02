use std::path::Path;

use gix::bstr::ByteSlice;
use serde::Serialize;

use crate::error::GitError;
use crate::ext::GitResultExt;
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
        None => repo.head_id().git_op()?.detach(),
    };

    let mut entries = Vec::with_capacity(limit);

    let walk = repo
        .rev_walk([start])
        .first_parent_only()
        .all()
        .git_op()?;

    for info in walk.take(limit) {
        let info = info.git_op()?;
        let id = info.id();
        let sha = id.to_string();

        let object = info.id().object().git_op()?;

        let commit = object.try_into_commit().git_op()?;

        let timestamp = commit.time().git_op()?.seconds;
        let author = commit.author().git_op()?.name.to_str_lossy().into_owned();
        let subject = commit
            .message()
            .git_op()?
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
