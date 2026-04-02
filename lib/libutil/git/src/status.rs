use std::ops::ControlFlow;
use std::path::Path;

use gix::bstr::BString;
use serde::Serialize;

use crate::error::GitError;
use crate::ext::GitResultExt;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct FileStatus {
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusInfo {
    pub staged: Vec<FileStatus>,
    pub unstaged: Vec<FileStatus>,
    pub untracked: Vec<FileStatus>,
}

pub fn collect(cwd: &Path) -> Result<StatusInfo, GitError> {
    let repo = open_repo(cwd)?;

    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    let patterns: Vec<BString> = Vec::new();
    let status_iter = repo
        .status(gix::progress::Discard)
        .git_op()?
        .into_index_worktree_iter(patterns)
        .git_op()?;

    for item in status_iter {
        let item = item.git_op()?;
        use gix::status::index_worktree::Item;
        match item {
            Item::Modification { rela_path, .. } => {
                unstaged.push(FileStatus {
                    path: rela_path.to_string(),
                });
            }
            Item::DirectoryContents { entry, .. } => {
                untracked.push(FileStatus {
                    path: entry.rela_path.to_string(),
                });
            }
            Item::Rewrite { .. } => {}
        }
    }

    // For staged changes, compare HEAD tree to index
    let staged = collect_staged(&repo)?;

    Ok(StatusInfo {
        staged,
        unstaged,
        untracked,
    })
}

fn collect_staged(repo: &gix::Repository) -> Result<Vec<FileStatus>, GitError> {
    let mut staged = Vec::new();

    let head_tree_id = match repo.head_tree_id() {
        Ok(id) => id,
        Err(_) => return Ok(staged), // No HEAD yet (empty repo)
    };

    let index = repo.index_or_empty().git_op()?;

    repo.tree_index_status(
        head_tree_id.as_ref(),
        &index,
        None,
        gix::status::tree_index::TrackRenames::Disabled,
        |change, _, _| {
            use gix::diff::index::ChangeRef;
            let path = match &change {
                ChangeRef::Addition { location, .. } => location.to_string(),
                ChangeRef::Deletion { location, .. } => location.to_string(),
                ChangeRef::Modification { location, .. } => location.to_string(),
                ChangeRef::Rewrite { location, .. } => location.to_string(),
            };
            staged.push(FileStatus { path });
            Ok::<_, std::convert::Infallible>(ControlFlow::Continue(()))
        },
    )
    .git_op()?;

    Ok(staged)
}
