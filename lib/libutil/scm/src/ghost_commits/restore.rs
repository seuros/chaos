use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use crate::GitToolingError;
use crate::operations::run_git_for_status;

use super::options::UntrackedSnapshot;

/// Restores the working tree and index to the given commit using `git restore`.
/// The repository root and optional repository-relative prefix limit the restore scope.
pub(super) fn restore_to_commit_inner(
    repo_root: &Path,
    repo_prefix: Option<&Path>,
    commit_id: &str,
) -> Result<(), GitToolingError> {
    // `git restore` resets the working tree to the snapshot commit.
    // We intentionally avoid --staged to preserve user's staged changes.
    // While this might leave some Chaos-staged changes in the index (if Chaos ran `git add`),
    // it prevents data loss for users who use the index as a save point.
    // Data safety > cleanliness.
    // Example:
    //   git restore --source <commit> --worktree -- <prefix>
    let mut restore_args = vec![
        OsString::from("restore"),
        OsString::from("--source"),
        OsString::from(commit_id),
        OsString::from("--worktree"),
        OsString::from("--"),
    ];
    if let Some(prefix) = repo_prefix {
        restore_args.push(prefix.as_os_str().to_os_string());
    } else {
        restore_args.push(OsString::from("."));
    }

    run_git_for_status(repo_root, restore_args, /*env*/ None)?;
    Ok(())
}

/// Removes untracked files and directories that were not present when the snapshot was captured.
pub(super) fn remove_new_untracked(
    repo_root: &Path,
    preserved_files: &[PathBuf],
    preserved_dirs: &[PathBuf],
    current: UntrackedSnapshot,
) -> Result<(), GitToolingError> {
    if current.files.is_empty() && current.dirs.is_empty() {
        return Ok(());
    }

    let preserved_file_set: HashSet<PathBuf> = preserved_files.iter().cloned().collect();
    let preserved_dirs_vec: Vec<PathBuf> = preserved_dirs.to_vec();

    for path in current.files {
        if should_preserve(&path, &preserved_file_set, &preserved_dirs_vec) {
            continue;
        }
        remove_path(&repo_root.join(&path))?;
    }

    for dir in current.dirs {
        if should_preserve(&dir, &preserved_file_set, &preserved_dirs_vec) {
            continue;
        }
        remove_path(&repo_root.join(&dir))?;
    }

    Ok(())
}

/// Determines whether an untracked path should be kept because it existed in the snapshot.
fn should_preserve(
    path: &Path,
    preserved_files: &HashSet<PathBuf>,
    preserved_dirs: &[PathBuf],
) -> bool {
    if preserved_files.contains(path) {
        return true;
    }

    preserved_dirs
        .iter()
        .any(|dir| path.starts_with(dir.as_path()))
}

/// Deletes the file or directory at the provided path, ignoring if it is already absent.
fn remove_path(path: &Path) -> Result<(), GitToolingError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
        Err(err) => {
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(err.into());
        }
    }
    Ok(())
}

pub(super) fn merge_preserved_untracked_files(
    mut files: Vec<PathBuf>,
    ignored: &[super::options::IgnoredUntrackedFile],
) -> Vec<PathBuf> {
    if ignored.is_empty() {
        return files;
    }

    files.extend(ignored.iter().map(|entry| entry.path.clone()));
    files
}

pub(super) fn merge_preserved_untracked_dirs(
    mut dirs: Vec<PathBuf>,
    ignored_large_dirs: &[super::options::LargeUntrackedDir],
) -> Vec<PathBuf> {
    if ignored_large_dirs.is_empty() {
        return dirs;
    }

    for entry in ignored_large_dirs {
        if dirs.iter().any(|dir| dir == &entry.path) {
            continue;
        }
        dirs.push(entry.path.clone());
    }

    dirs
}
