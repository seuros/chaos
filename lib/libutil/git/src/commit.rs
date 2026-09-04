use std::ops::ControlFlow;
use std::path::Path;

use gix::refs::Target;
use gix::refs::transaction::Change;
use gix::refs::transaction::LogChange;
use gix::refs::transaction::PreviousValue;
use gix::refs::transaction::RefEdit;
use gix::refs::transaction::RefLog;
use serde::Serialize;

use crate::error::GitError;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct CommitResult {
    pub sha: String,
    pub branch: Option<String>,
    pub detached: bool,
    pub subject: String,
    pub committed_paths: Vec<String>,
}

pub fn commit(cwd: &Path, message: &str) -> Result<CommitResult, GitError> {
    commit_inner(cwd, message, false)
}

pub fn amend(cwd: &Path, message: &str) -> Result<CommitResult, GitError> {
    commit_inner(cwd, message, true)
}

fn commit_inner(cwd: &Path, message: &str, amend: bool) -> Result<CommitResult, GitError> {
    let message = message.trim();
    if message.is_empty() {
        return Err(GitError::InvalidInput(
            "commit message must not be empty".to_string(),
        ));
    }

    let repo = open_repo(cwd)?;
    if let Some(state) = repo.state() {
        return Err(GitError::RepositoryState(format!("{state:?}")));
    }

    let index = repo
        .index_or_load_from_head_or_empty()
        .map_err(|e| GitError::Operation(e.to_string()))?
        .into_owned();
    reject_unsupported_index_entries(&index)?;

    let head = repo
        .head()
        .map_err(|e| GitError::Operation(e.to_string()))?;
    let parent_id = head.id().map(gix::Id::detach);
    let detached = head.is_detached();
    let branch = head.referent_name().map(|name| name.shorten().to_string());
    let amend_head_id = match (amend, parent_id) {
        (true, Some(head_id)) => Some(head_id),
        (true, None) => {
            return Err(GitError::InvalidInput(
                "cannot amend because HEAD has no commit".to_string(),
            ));
        }
        (false, _) => None,
    };

    let head_tree_id = match parent_id {
        Some(parent) => {
            let commit = repo
                .find_commit(parent)
                .map_err(|e| GitError::Operation(e.to_string()))?;
            commit
                .tree_id()
                .map(gix::Id::detach)
                .map_err(|e| GitError::Operation(e.to_string()))?
        }
        None => repo.empty_tree().id,
    };

    let mut committed_paths = collect_staged_paths(&repo, &index, &head_tree_id)?;
    if committed_paths.is_empty() && !amend {
        return Err(GitError::EmptyCommit);
    }
    committed_paths.sort();
    committed_paths.dedup();

    let mut editor = repo
        .edit_tree(repo.empty_tree().id)
        .map_err(|e| GitError::Operation(e.to_string()))?;
    for entry in index.entries() {
        let kind = match entry.mode {
            mode if mode == gix::index::entry::Mode::FILE => gix::objs::tree::EntryKind::Blob,
            mode if mode == gix::index::entry::Mode::FILE_EXECUTABLE => {
                gix::objs::tree::EntryKind::BlobExecutable
            }
            mode if mode == gix::index::entry::Mode::SYMLINK => gix::objs::tree::EntryKind::Link,
            mode if mode == gix::index::entry::Mode::COMMIT => gix::objs::tree::EntryKind::Commit,
            mode if mode == gix::index::entry::Mode::DIR => {
                return Err(GitError::Unsupported(
                    "sparse indexes are not supported".to_string(),
                ));
            }
            _ => {
                return Err(GitError::Unsupported(format!(
                    "unsupported index mode for {}",
                    entry.path(&index)
                )));
            }
        };
        editor
            .upsert(entry.path(&index), kind, entry.id)
            .map_err(|e| GitError::Operation(e.to_string()))?;
    }
    let tree_id = editor
        .write()
        .map_err(|e| GitError::Operation(e.to_string()))?
        .detach();

    let commit_id = if let Some(head_id) = amend_head_id {
        amend_head(&repo, &head, head_id, message, tree_id)?
    } else {
        repo.commit("HEAD", message, tree_id, parent_id)
            .map(gix::Id::detach)
            .map_err(|e| GitError::Operation(e.to_string()))?
    };

    Ok(CommitResult {
        sha: commit_id.to_string(),
        branch,
        detached,
        subject: message.lines().next().unwrap_or_default().to_string(),
        committed_paths,
    })
}

fn amend_head(
    repo: &gix::Repository,
    head: &gix::Head<'_>,
    head_id: gix::ObjectId,
    message: &str,
    tree_id: gix::ObjectId,
) -> Result<gix::ObjectId, GitError> {
    let previous = repo
        .find_commit(head_id)
        .map_err(|error| GitError::Operation(error.to_string()))?;
    let parents = previous
        .parent_ids()
        .map(gix::Id::detach)
        .collect::<Vec<_>>();
    let author = previous
        .author()
        .map_err(|error| GitError::Operation(error.to_string()))?;
    let committer = repo
        .committer()
        .ok_or_else(|| GitError::Operation("committer identity is missing".to_string()))?
        .map_err(|error| GitError::Operation(error.to_string()))?;
    let commit_id = repo
        .new_commit_as(committer, author, message, tree_id, parents)
        .map(|commit| commit.id().detach())
        .map_err(|error| GitError::Operation(error.to_string()))?;
    let subject = message.lines().next().unwrap_or_default();

    repo.edit_references_as(
        Some(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("commit (amend): {subject}").into(),
                },
                expected: PreviousValue::MustExistAndMatch(Target::Object(head_id)),
                new: Target::Object(commit_id),
            },
            name: head.name().to_owned(),
            deref: true,
        }),
        Some(committer),
    )
    .map_err(|error| GitError::Operation(error.to_string()))?;

    Ok(commit_id)
}

fn reject_unsupported_index_entries(index: &gix::index::File) -> Result<(), GitError> {
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            return Err(GitError::Conflict(entry.path(index).to_string()));
        }
        if entry.mode == gix::index::entry::Mode::DIR {
            return Err(GitError::Unsupported(
                "sparse indexes are not supported".to_string(),
            ));
        }
    }
    Ok(())
}

fn collect_staged_paths(
    repo: &gix::Repository,
    index: &gix::index::File,
    head_tree_id: &gix::hash::ObjectId,
) -> Result<Vec<String>, GitError> {
    let mut paths = Vec::new();
    let mut changed_submodule = None;
    repo.tree_index_status(
        head_tree_id.as_ref(),
        index,
        None,
        gix::status::tree_index::TrackRenames::Disabled,
        |change, _, _| {
            use gix::diff::index::ChangeRef;
            let (path, is_submodule_change) = match change {
                ChangeRef::Addition {
                    location,
                    entry_mode,
                    ..
                }
                | ChangeRef::Deletion {
                    location,
                    entry_mode,
                    ..
                } => (
                    location.to_string(),
                    entry_mode == gix::index::entry::Mode::COMMIT,
                ),
                ChangeRef::Modification {
                    location,
                    previous_entry_mode,
                    entry_mode,
                    ..
                } => (
                    location.to_string(),
                    previous_entry_mode == gix::index::entry::Mode::COMMIT
                        || entry_mode == gix::index::entry::Mode::COMMIT,
                ),
                ChangeRef::Rewrite {
                    location,
                    source_entry_mode,
                    entry_mode,
                    ..
                } => (
                    location.to_string(),
                    source_entry_mode == gix::index::entry::Mode::COMMIT
                        || entry_mode == gix::index::entry::Mode::COMMIT,
                ),
            };
            if is_submodule_change {
                changed_submodule = Some(path);
                return Ok::<_, std::convert::Infallible>(ControlFlow::Break(()));
            }
            paths.push(path);
            Ok::<_, std::convert::Infallible>(ControlFlow::Continue(()))
        },
    )
    .map_err(|e| GitError::Operation(e.to_string()))?;
    if let Some(path) = changed_submodule {
        return Err(GitError::Unsupported(format!(
            "staged submodule changes are not supported: {path}"
        )));
    }
    Ok(paths)
}
