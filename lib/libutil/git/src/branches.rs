use std::path::Path;

use serde::Serialize;

use crate::error::GitError;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct BranchInfo {
    pub current: Option<String>,
    pub default: Option<String>,
    pub local: Vec<String>,
    pub remote: Vec<String>,
}

pub fn collect(cwd: &Path) -> Result<BranchInfo, GitError> {
    let repo = open_repo(cwd)?;

    let current = repo
        .head_ref()
        .ok()
        .flatten()
        .map(|r| r.name().shorten().to_string());

    let default = crate::repo::info(cwd)
        .ok()
        .and_then(|info| info.default_branch);

    let mut local = Vec::new();
    let mut remote = Vec::new();

    if let Ok(refs) = repo.references() {
        if let Ok(local_iter) = refs.local_branches() {
            for r in local_iter.flatten() {
                local.push(r.name().shorten().to_string());
            }
        }
        if let Ok(remote_iter) = refs.remote_branches() {
            for r in remote_iter.flatten() {
                remote.push(r.name().shorten().to_string());
            }
        }
    }

    local.sort_unstable();
    remote.sort_unstable();

    // Put default branch first in local list
    if let Some(ref def) = default {
        if let Some(pos) = local.iter().position(|b| b == def) {
            let branch = local.remove(pos);
            local.insert(0, branch);
        }
    }

    Ok(BranchInfo {
        current,
        default,
        local,
        remote,
    })
}
