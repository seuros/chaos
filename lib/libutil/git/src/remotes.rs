use std::collections::BTreeMap;
use std::path::Path;

use gix::bstr::ByteSlice;
use serde::Serialize;

use crate::error::GitError;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct RemoteInfo {
    pub remotes: BTreeMap<String, String>,
}

pub fn collect(cwd: &Path) -> Result<RemoteInfo, GitError> {
    let repo = open_repo(cwd)?;

    let mut remotes = BTreeMap::new();

    for name in repo.remote_names() {
        let name_bstr = name.as_bstr();
        if let Ok(remote) = repo.find_remote(name_bstr)
            && let Some(url) = remote.url(gix::remote::Direction::Fetch)
        {
            remotes.insert(name.to_string(), url.to_bstring().to_string());
        }
    }

    Ok(RemoteInfo { remotes })
}
