use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;

use tempfile::Builder;

use crate::GhostCommit;
use crate::GitToolingError;
use crate::operations::apply_repo_prefix_to_force_include;
use crate::operations::ensure_git_repository;
use crate::operations::normalize_relative_path;
use crate::operations::repo_subdir;
use crate::operations::resolve_head;
use crate::operations::resolve_repository_root;
use crate::operations::run_git_for_status;
use crate::operations::run_git_for_stdout;

use super::capture::capture_existing_untracked;
use super::capture::capture_status_snapshot;
use super::git_ops::add_paths_to_index;
use super::git_ops::default_commit_identity;
use super::options::CreateGhostCommitOptions;
use super::options::GhostSnapshotReport;
use super::options::IgnoredUntrackedFile;
use super::options::LargeUntrackedDir;
use super::options::RestoreGhostCommitOptions;
use super::restore::merge_preserved_untracked_dirs;
use super::restore::merge_preserved_untracked_files;
use super::restore::remove_new_untracked;
use super::restore::restore_to_commit_inner;

/// Default commit message used for ghost commits when none is provided.
const DEFAULT_COMMIT_MESSAGE: &str = "chaos snapshot";

pub(super) fn to_session_relative_path(path: &Path, repo_prefix: Option<&Path>) -> PathBuf {
    match repo_prefix {
        Some(prefix) => path
            .strip_prefix(prefix)
            .map(PathBuf::from)
            .unwrap_or_else(|_| path.to_path_buf()),
        None => path.to_path_buf(),
    }
}

pub(super) fn prepare_force_include(
    repo_prefix: Option<&Path>,
    force_include: &[PathBuf],
) -> Result<Vec<PathBuf>, GitToolingError> {
    let normalized_force = force_include
        .iter()
        .map(PathBuf::as_path)
        .map(normalize_relative_path)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(apply_repo_prefix_to_force_include(
        repo_prefix,
        &normalized_force,
    ))
}

pub(super) fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            result.push(path);
        }
    }
    result
}

/// Create a ghost commit capturing the current state of the repository's working tree.
pub fn create_ghost_commit(
    options: &CreateGhostCommitOptions<'_>,
) -> Result<GhostCommit, GitToolingError> {
    create_ghost_commit_with_report(options).map(|(commit, _)| commit)
}

/// Compute a report describing the working tree for a ghost snapshot without creating a commit.
pub fn capture_ghost_snapshot_report(
    options: &CreateGhostCommitOptions<'_>,
) -> Result<GhostSnapshotReport, GitToolingError> {
    ensure_git_repository(options.repo_path)?;

    let repo_root = resolve_repository_root(options.repo_path)?;
    let repo_prefix = repo_subdir(repo_root.as_path(), options.repo_path);
    let force_include = prepare_force_include(repo_prefix.as_deref(), &options.force_include)?;
    let existing_untracked = capture_existing_untracked(
        repo_root.as_path(),
        repo_prefix.as_deref(),
        options.ghost_snapshot.ignore_large_untracked_files,
        options.ghost_snapshot.ignore_large_untracked_dirs,
        &force_include,
    )?;

    let warning_ignored_files = existing_untracked
        .ignored_untracked_files
        .iter()
        .map(|file| IgnoredUntrackedFile {
            path: to_session_relative_path(file.path.as_path(), repo_prefix.as_deref()),
            byte_size: file.byte_size,
        })
        .collect::<Vec<_>>();
    let warning_ignored_dirs = existing_untracked
        .ignored_large_untracked_dirs
        .iter()
        .map(|dir| LargeUntrackedDir {
            path: to_session_relative_path(dir.path.as_path(), repo_prefix.as_deref()),
            file_count: dir.file_count,
        })
        .collect::<Vec<_>>();

    Ok(GhostSnapshotReport {
        large_untracked_dirs: warning_ignored_dirs,
        ignored_untracked_files: warning_ignored_files,
    })
}

/// Create a ghost commit capturing the current state of the repository's working tree along with a report.
pub fn create_ghost_commit_with_report(
    options: &CreateGhostCommitOptions<'_>,
) -> Result<(GhostCommit, GhostSnapshotReport), GitToolingError> {
    ensure_git_repository(options.repo_path)?;

    let repo_root = resolve_repository_root(options.repo_path)?;
    let repo_prefix = repo_subdir(repo_root.as_path(), options.repo_path);
    let parent = resolve_head(repo_root.as_path())?;
    let force_include = prepare_force_include(repo_prefix.as_deref(), &options.force_include)?;
    let status_snapshot = capture_status_snapshot(
        repo_root.as_path(),
        repo_prefix.as_deref(),
        options.ghost_snapshot.ignore_large_untracked_files,
        options.ghost_snapshot.ignore_large_untracked_dirs,
        &force_include,
    )?;
    let existing_untracked = status_snapshot.untracked;

    let warning_ignored_files = existing_untracked
        .ignored_untracked_files
        .iter()
        .map(|file| IgnoredUntrackedFile {
            path: to_session_relative_path(file.path.as_path(), repo_prefix.as_deref()),
            byte_size: file.byte_size,
        })
        .collect::<Vec<_>>();
    let large_untracked_dirs = existing_untracked
        .ignored_large_untracked_dirs
        .iter()
        .map(|dir| LargeUntrackedDir {
            path: to_session_relative_path(dir.path.as_path(), repo_prefix.as_deref()),
            file_count: dir.file_count,
        })
        .collect::<Vec<_>>();
    let index_tempdir = Builder::new().prefix("chaos-git-index-").tempdir()?;
    let index_path = index_tempdir.path().join("index");
    let base_env = vec![(
        OsString::from("GIT_INDEX_FILE"),
        OsString::from(index_path.as_os_str()),
    )];
    // Use a temporary index so snapshotting does not disturb the user's index state.
    // Example plumbing sequence:
    //   GIT_INDEX_FILE=/tmp/index git read-tree HEAD
    //   GIT_INDEX_FILE=/tmp/index git add --all -- <paths>
    //   GIT_INDEX_FILE=/tmp/index git write-tree
    //   GIT_INDEX_FILE=/tmp/index git commit-tree <tree> -p <parent> -m "chaos snapshot"

    // Pre-populate the temporary index with HEAD so unchanged tracked files
    // are included in the snapshot tree.
    if let Some(parent_sha) = parent.as_deref() {
        run_git_for_status(
            repo_root.as_path(),
            vec![OsString::from("read-tree"), OsString::from(parent_sha)],
            Some(base_env.as_slice()),
        )?;
    }

    let mut index_paths = status_snapshot.tracked_paths;
    index_paths.extend(existing_untracked.untracked_files_for_index.iter().cloned());
    let index_paths = dedupe_paths(index_paths);
    // Stage tracked + new files into the temp index so write-tree reflects the working tree.
    // We use `git add --all` to make deletions show up in the snapshot tree too.
    add_paths_to_index(repo_root.as_path(), base_env.as_slice(), &index_paths)?;
    if !force_include.is_empty() {
        let mut args = Vec::with_capacity(force_include.len() + 2);
        args.push(OsString::from("add"));
        args.push(OsString::from("--force"));
        args.extend(
            force_include
                .iter()
                .map(|path| OsString::from(path.as_os_str())),
        );
        run_git_for_status(repo_root.as_path(), args, Some(base_env.as_slice()))?;
    }

    let tree_id = run_git_for_stdout(
        repo_root.as_path(),
        vec![OsString::from("write-tree")],
        Some(base_env.as_slice()),
    )?;

    let mut commit_env = base_env;
    commit_env.extend(default_commit_identity());
    let message = options.message.unwrap_or(DEFAULT_COMMIT_MESSAGE);
    let commit_args = {
        let mut result = vec![OsString::from("commit-tree"), OsString::from(&tree_id)];
        if let Some(parent) = parent.as_deref() {
            result.extend([OsString::from("-p"), OsString::from(parent)]);
        }
        result.extend([OsString::from("-m"), OsString::from(message)]);
        result
    };

    // `git commit-tree` writes a detached commit object without updating refs,
    // which keeps snapshots out of the user's branch history.
    // Retrieve commit ID.
    let commit_id = run_git_for_stdout(
        repo_root.as_path(),
        commit_args,
        Some(commit_env.as_slice()),
    )?;

    let ghost_commit = GhostCommit::new(
        commit_id,
        parent,
        merge_preserved_untracked_files(
            existing_untracked.files,
            &existing_untracked.ignored_untracked_files,
        ),
        merge_preserved_untracked_dirs(
            existing_untracked.dirs,
            &existing_untracked.ignored_large_untracked_dirs,
        ),
    );

    Ok((
        ghost_commit,
        GhostSnapshotReport {
            large_untracked_dirs,
            ignored_untracked_files: warning_ignored_files,
        },
    ))
}

/// Restore the working tree to match the provided ghost commit.
pub fn restore_ghost_commit(repo_path: &Path, commit: &GhostCommit) -> Result<(), GitToolingError> {
    restore_ghost_commit_with_options(&RestoreGhostCommitOptions::new(repo_path), commit)
}

/// Restore the working tree using the provided options.
pub fn restore_ghost_commit_with_options(
    options: &RestoreGhostCommitOptions<'_>,
    commit: &GhostCommit,
) -> Result<(), GitToolingError> {
    ensure_git_repository(options.repo_path)?;

    let repo_root = resolve_repository_root(options.repo_path)?;
    let repo_prefix = repo_subdir(repo_root.as_path(), options.repo_path);
    let current_untracked = capture_existing_untracked(
        repo_root.as_path(),
        repo_prefix.as_deref(),
        options.ghost_snapshot.ignore_large_untracked_files,
        options.ghost_snapshot.ignore_large_untracked_dirs,
        &[],
    )?;
    restore_to_commit_inner(repo_root.as_path(), repo_prefix.as_deref(), commit.id())?;
    remove_new_untracked(
        repo_root.as_path(),
        commit.preexisting_untracked_files(),
        commit.preexisting_untracked_dirs(),
        current_untracked,
    )
}

/// Restore the working tree to match the given commit ID.
pub fn restore_to_commit(repo_path: &Path, commit_id: &str) -> Result<(), GitToolingError> {
    ensure_git_repository(repo_path)?;

    let repo_root = resolve_repository_root(repo_path)?;
    let repo_prefix = repo_subdir(repo_root.as_path(), repo_path);
    restore_to_commit_inner(repo_root.as_path(), repo_prefix.as_deref(), commit_id)
}
