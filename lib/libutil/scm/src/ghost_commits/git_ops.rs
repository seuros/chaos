use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;

use crate::GitToolingError;
use crate::operations::run_git_for_status;

pub(super) fn add_paths_to_index(
    repo_root: &Path,
    env: &[(OsString, OsString)],
    paths: &[PathBuf],
) -> Result<(), GitToolingError> {
    if paths.is_empty() {
        return Ok(());
    }

    let chunk_size = usize::try_from(64_i64).unwrap_or(1);
    for chunk in paths.chunks(chunk_size) {
        let mut args = vec![
            OsString::from("add"),
            OsString::from("--all"),
            OsString::from("--"),
        ];
        args.extend(chunk.iter().map(|path| path.as_os_str().to_os_string()));
        // Chunk the argv to avoid oversized command lines on large repos.
        run_git_for_status(repo_root, args, Some(env))?;
    }

    Ok(())
}

/// Returns the default author and committer identity for ghost commits.
pub(super) fn default_commit_identity() -> Vec<(OsString, OsString)> {
    vec![
        (
            OsString::from("GIT_AUTHOR_NAME"),
            OsString::from("Chaos Snapshot"),
        ),
        (
            OsString::from("GIT_AUTHOR_EMAIL"),
            OsString::from("snapshot@chaos.local"),
        ),
        (
            OsString::from("GIT_COMMITTER_NAME"),
            OsString::from("Chaos Snapshot"),
        ),
        (
            OsString::from("GIT_COMMITTER_EMAIL"),
            OsString::from("snapshot@chaos.local"),
        ),
    ]
}
