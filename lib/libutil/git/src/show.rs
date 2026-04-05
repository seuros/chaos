use std::path::Path;

use gix::bstr::ByteSlice;
use serde::Deserialize;
use serde::Serialize;

use crate::error::GitError;
use crate::ext::GitResultExt;
use crate::open_repo;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitTrailer {
    pub token: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowEntry {
    pub sha: String,
    pub timestamp: i64,
    pub author: String,
    pub subject: String,
    pub body: String,
    pub trailers: Vec<CommitTrailer>,
}

/// Show one commit with full message details.
///
/// - `rev`: revision to resolve (default: HEAD)
pub fn show(cwd: &Path, rev: Option<&str>) -> Result<ShowEntry, GitError> {
    let repo = open_repo(cwd)?;
    let rev = rev.unwrap_or("HEAD");

    let object = repo
        .rev_parse_single(rev)
        .map_err(|e| GitError::RefNotFound(format!("{rev}: {e}")))?
        .object()
        .git_op()?;
    let commit = object.try_into_commit().git_op()?;
    let message = commit.message().git_op()?;

    let body = message
        .body()
        .map(|body| body.without_trailer().to_str_lossy().into_owned())
        .unwrap_or_default();
    let trailers = message
        .body()
        .map(|body| {
            body.trailers()
                .map(|trailer| CommitTrailer {
                    token: trailer.token.to_str_lossy().into_owned(),
                    value: trailer.value.to_str_lossy().into_owned(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ShowEntry {
        sha: commit.id().to_string(),
        timestamp: commit.time().git_op()?.seconds,
        author: commit.author().git_op()?.name.to_str_lossy().into_owned(),
        subject: message.title.to_str_lossy().into_owned(),
        body,
        trailers,
    })
}
