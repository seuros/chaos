use std::path::Path;
use std::path::PathBuf;

use serde::Serialize;

use crate::error::GitError;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct RepoInfo {
    pub root: PathBuf,
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub remote_url: Option<String>,
    pub has_changes: bool,
    pub default_branch: Option<String>,
}

pub fn info(cwd: &Path) -> Result<RepoInfo, GitError> {
    let repo = open_repo(cwd)?;

    let root = repo
        .workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf();

    let head_sha = repo.head_id().ok().map(|id| id.to_string());

    let branch = repo
        .head_ref()
        .ok()
        .flatten()
        .map(|r| r.name().shorten().to_string());

    let remote_url = repo
        .find_default_remote(gix::remote::Direction::Fetch)
        .and_then(std::result::Result::ok)
        .and_then(|remote| {
            remote
                .url(gix::remote::Direction::Fetch)
                .map(|u| u.to_bstring().to_string())
        });

    let has_changes = repo.is_dirty().unwrap_or(false);

    let default_branch = detect_default_branch(&repo);

    Ok(RepoInfo {
        root,
        head_sha,
        branch,
        remote_url,
        has_changes,
        default_branch,
    })
}

fn detect_default_branch(repo: &gix::Repository) -> Option<String> {
    // Try remote HEAD symbolic ref first
    if let Some(Ok(remote)) = repo.find_default_remote(gix::remote::Direction::Fetch) {
        let remote_name = remote.name()?.as_bstr().to_string();
        let head_ref_name = format!("refs/remotes/{remote_name}/HEAD");
        if let Ok(reference) = repo.find_reference(&head_ref_name)
            && let Some(target) = reference.target().try_name()
        {
            let name = target.as_bstr().to_string();
            if let Some(branch) = name.rsplit('/').next() {
                return Some(branch.to_owned());
            }
        }
    }

    // Fallback: check for common local defaults
    for candidate in ["main", "master"] {
        let refname = format!("refs/heads/{candidate}");
        if repo.find_reference(&refname).is_ok() {
            return Some(candidate.to_string());
        }
    }

    None
}
