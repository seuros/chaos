use std::path::Path;

use gix::bstr::ByteSlice;
use gix::refs::FullName;
use gix::refs::transaction::PreviousValue;
use serde::Serialize;

use crate::error::GitError;
use crate::ext::GitResultExt;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct BranchMutationResult {
    pub operation: &'static str,
    pub branch: String,
    pub oid: String,
}

fn full_branch_name(name: &str) -> Result<FullName, GitError> {
    if name.trim() != name || name.is_empty() {
        return Err(GitError::InvalidInput(
            "branch name must be non-empty and contain no surrounding whitespace".to_string(),
        ));
    }
    if name.starts_with("refs/") {
        return Err(GitError::InvalidInput(
            "branch name must be a short local name without a refs/heads/ prefix".to_string(),
        ));
    }

    let full = format!("refs/heads/{name}");
    gix::validate::reference::branch_name(full.as_bytes().as_bstr()).map_err(|error| {
        GitError::InvalidInput(format!("invalid branch name {name:?}: {error}"))
    })?;
    full.try_into()
        .map_err(|error| GitError::InvalidInput(format!("invalid branch name {name:?}: {error}")))
}

pub fn create(
    cwd: &Path,
    name: &str,
    start_point: Option<&str>,
) -> Result<BranchMutationResult, GitError> {
    let repo = open_repo(cwd)?;
    let full_name = full_branch_name(name)?;
    let start_point = start_point.unwrap_or("HEAD");
    let target = repo
        .rev_parse_single(start_point)
        .map_err(|error| GitError::RefNotFound(format!("{start_point}: {error}")))?
        .object()
        .git_op()?
        .peel_to_commit()
        .git_op()?
        .id()
        .detach();

    repo.reference(
        full_name,
        target,
        PreviousValue::MustNotExist,
        format!("branch: Created from {start_point}"),
    )
    .git_op()?;

    Ok(BranchMutationResult {
        operation: "create",
        branch: name.to_string(),
        oid: target.to_string(),
    })
}

pub fn delete(cwd: &Path, name: &str, force: bool) -> Result<BranchMutationResult, GitError> {
    let mut repo = open_repo(cwd)?;
    let full_name = full_branch_name(name)?;
    let branch_id = repo
        .find_reference(&full_name)
        .map_err(|error| GitError::RefNotFound(format!("{name}: {error}")))?
        .id()
        .detach();

    if !force {
        let head_id = repo.head_commit().git_op()?.id().detach();
        let merge_base = repo.merge_base(branch_id, head_id).git_op()?.detach();
        if merge_base != branch_id {
            return Err(GitError::Operation(format!(
                "branch {name:?} is not fully merged into HEAD; pass force=true to delete it"
            )));
        }
    }

    repo.delete_local_branches([full_name]).git_op()?;

    Ok(BranchMutationResult {
        operation: "delete",
        branch: name.to_string(),
        oid: branch_id.to_string(),
    })
}

#[cfg(test)]
mod tests;
