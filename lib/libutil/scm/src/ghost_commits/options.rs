use std::path::Path;
use std::path::PathBuf;

/// Default threshold for ignoring large untracked directories.
pub(super) const DEFAULT_IGNORE_LARGE_UNTRACKED_DIRS: i64 = 200;
/// Default threshold (10 MiB) for excluding large untracked files from ghost snapshots.
pub(super) const DEFAULT_IGNORE_LARGE_UNTRACKED_FILES: i64 = 10 * 1024 * 1024;

/// Options to control ghost commit creation.
pub struct CreateGhostCommitOptions<'a> {
    pub repo_path: &'a Path,
    pub message: Option<&'a str>,
    pub force_include: Vec<PathBuf>,
    pub ghost_snapshot: GhostSnapshotConfig,
}

/// Options to control ghost commit restoration.
pub struct RestoreGhostCommitOptions<'a> {
    pub repo_path: &'a Path,
    pub ghost_snapshot: GhostSnapshotConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostSnapshotConfig {
    pub ignore_large_untracked_files: Option<i64>,
    pub ignore_large_untracked_dirs: Option<i64>,
    pub disable_warnings: bool,
}

impl Default for GhostSnapshotConfig {
    fn default() -> Self {
        Self {
            ignore_large_untracked_files: Some(DEFAULT_IGNORE_LARGE_UNTRACKED_FILES),
            ignore_large_untracked_dirs: Some(DEFAULT_IGNORE_LARGE_UNTRACKED_DIRS),
            disable_warnings: false,
        }
    }
}

/// Summary produced alongside a ghost snapshot.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GhostSnapshotReport {
    pub large_untracked_dirs: Vec<LargeUntrackedDir>,
    pub ignored_untracked_files: Vec<IgnoredUntrackedFile>,
}

/// Directory containing a large amount of untracked content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LargeUntrackedDir {
    pub path: PathBuf,
    pub file_count: i64,
}

/// Untracked file excluded from the snapshot because of its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredUntrackedFile {
    pub path: PathBuf,
    pub byte_size: i64,
}

impl<'a> CreateGhostCommitOptions<'a> {
    /// Creates options scoped to the provided repository path.
    pub fn new(repo_path: &'a Path) -> Self {
        Self {
            repo_path,
            message: None,
            force_include: Vec::new(),
            ghost_snapshot: GhostSnapshotConfig::default(),
        }
    }

    /// Sets a custom commit message for the ghost commit.
    pub fn message(mut self, message: &'a str) -> Self {
        self.message = Some(message);
        self
    }

    pub fn ghost_snapshot(mut self, ghost_snapshot: GhostSnapshotConfig) -> Self {
        self.ghost_snapshot = ghost_snapshot;
        self
    }

    /// Exclude untracked files larger than `bytes` from the snapshot commit.
    ///
    /// These files are still treated as untracked for preservation purposes (i.e. they will not be
    /// deleted by undo), but they will not be captured in the snapshot tree.
    pub fn ignore_large_untracked_files(mut self, bytes: i64) -> Self {
        if bytes > 0 {
            self.ghost_snapshot.ignore_large_untracked_files = Some(bytes);
        } else {
            self.ghost_snapshot.ignore_large_untracked_files = None;
        }
        self
    }

    /// Supplies the entire force-include path list at once.
    pub fn force_include<I>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        self.force_include = paths.into_iter().collect();
        self
    }

    /// Adds a single path to the force-include list.
    pub fn push_force_include<P>(mut self, path: P) -> Self
    where
        P: Into<PathBuf>,
    {
        self.force_include.push(path.into());
        self
    }
}

impl<'a> RestoreGhostCommitOptions<'a> {
    /// Creates restore options scoped to the provided repository path.
    pub fn new(repo_path: &'a Path) -> Self {
        Self {
            repo_path,
            ghost_snapshot: GhostSnapshotConfig::default(),
        }
    }

    pub fn ghost_snapshot(mut self, ghost_snapshot: GhostSnapshotConfig) -> Self {
        self.ghost_snapshot = ghost_snapshot;
        self
    }

    /// Exclude untracked files larger than `bytes` from undo cleanup.
    ///
    /// These files are treated as "always preserve" to avoid deleting large local artifacts.
    pub fn ignore_large_untracked_files(mut self, bytes: i64) -> Self {
        if bytes > 0 {
            self.ghost_snapshot.ignore_large_untracked_files = Some(bytes);
        } else {
            self.ghost_snapshot.ignore_large_untracked_files = None;
        }
        self
    }

    /// Ignore untracked directories that contain at least `file_count` untracked files.
    pub fn ignore_large_untracked_dirs(mut self, file_count: i64) -> Self {
        if file_count > 0 {
            self.ghost_snapshot.ignore_large_untracked_dirs = Some(file_count);
        } else {
            self.ghost_snapshot.ignore_large_untracked_dirs = None;
        }
        self
    }
}

/// Untracked snapshot collected before or during a ghost commit operation.
#[derive(Default)]
pub(super) struct UntrackedSnapshot {
    pub(super) files: Vec<PathBuf>,
    pub(super) dirs: Vec<PathBuf>,
    pub(super) untracked_files_for_index: Vec<PathBuf>,
    pub(super) ignored_untracked_files: Vec<IgnoredUntrackedFile>,
    pub(super) ignored_large_untracked_dirs: Vec<LargeUntrackedDir>,
    pub(super) ignored_large_untracked_dir_files: Vec<PathBuf>,
}

/// Full status snapshot including tracked paths and untracked state.
#[derive(Default)]
pub(super) struct StatusSnapshot {
    pub(super) tracked_paths: Vec<PathBuf>,
    pub(super) untracked: UntrackedSnapshot,
}
