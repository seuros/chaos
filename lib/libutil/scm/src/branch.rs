use std::ffi::OsString;
use std::path::Path;

use crate::GitToolingError;
use crate::operations::ensure_git_repository;
use crate::operations::resolve_head;
use crate::operations::resolve_repository_root;
use crate::operations::run_git_for_stdout;

/// Returns the merge-base commit between `HEAD` and the latest version between local
/// and remote of the provided branch, if both exist.
///
/// The function mirrors `git merge-base HEAD <branch>` but returns `Ok(None)` when
/// the repository has no `HEAD` yet or when the branch cannot be resolved.
pub fn merge_base_with_head(
    repo_path: &Path,
    branch: &str,
) -> Result<Option<String>, GitToolingError> {
    ensure_git_repository(repo_path)?;
    let repo_root = resolve_repository_root(repo_path)?;
    let head = match resolve_head(repo_root.as_path())? {
        Some(head) => head,
        None => return Ok(None),
    };

    let Some(branch_ref) = resolve_branch_ref(repo_root.as_path(), branch)? else {
        return Ok(None);
    };

    let preferred_ref =
        if let Some(upstream) = resolve_upstream_if_remote_ahead(repo_root.as_path(), branch)? {
            resolve_branch_ref(repo_root.as_path(), &upstream)?.unwrap_or(branch_ref)
        } else {
            branch_ref
        };

    let merge_base = run_git_for_stdout(
        repo_root.as_path(),
        vec![
            OsString::from("merge-base"),
            OsString::from(head),
            OsString::from(preferred_ref),
        ],
        /*env*/ None,
    )?;

    Ok(Some(merge_base))
}

fn resolve_branch_ref(repo_root: &Path, branch: &str) -> Result<Option<String>, GitToolingError> {
    let rev = run_git_for_stdout(
        repo_root,
        vec![
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from(branch),
        ],
        /*env*/ None,
    );

    match rev {
        Ok(rev) => Ok(Some(rev)),
        Err(GitToolingError::GitCommand { .. }) => Ok(None),
        Err(other) => Err(other),
    }
}

fn resolve_upstream_if_remote_ahead(
    repo_root: &Path,
    branch: &str,
) -> Result<Option<String>, GitToolingError> {
    let upstream = match run_git_for_stdout(
        repo_root,
        vec![
            OsString::from("rev-parse"),
            OsString::from("--abbrev-ref"),
            OsString::from("--symbolic-full-name"),
            OsString::from(format!("{branch}@{{upstream}}")),
        ],
        /*env*/ None,
    ) {
        Ok(name) => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed.to_string()
        }
        Err(GitToolingError::GitCommand { .. }) => return Ok(None),
        Err(other) => return Err(other),
    };

    let counts = match run_git_for_stdout(
        repo_root,
        vec![
            OsString::from("rev-list"),
            OsString::from("--left-right"),
            OsString::from("--count"),
            OsString::from(format!("{branch}...{upstream}")),
        ],
        /*env*/ None,
    ) {
        Ok(counts) => counts,
        Err(GitToolingError::GitCommand { .. }) => return Ok(None),
        Err(other) => return Err(other),
    };

    let mut parts = counts.split_whitespace();
    let _left: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let right: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);

    if right > 0 {
        Ok(Some(upstream))
    } else {
        Ok(None)
    }
}
