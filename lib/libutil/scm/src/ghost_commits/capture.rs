use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use crate::GitToolingError;
use crate::operations::normalize_relative_path;
use crate::operations::run_git_for_stdout_all;

use super::options::IgnoredUntrackedFile;
use super::options::LargeUntrackedDir;
use super::options::StatusSnapshot;
use super::options::UntrackedSnapshot;

/// Directories that should always be ignored when capturing ghost snapshots,
/// even if they are not listed in .gitignore.
///
/// These are typically large dependency or build trees that are not useful
/// for undo and can cause snapshots to grow without bound.
pub(super) const DEFAULT_IGNORED_DIR_NAMES: &[&str] = &[
    "node_modules",
    ".venv",
    "venv",
    "env",
    ".env",
    "dist",
    "build",
    ".pytest_cache",
    ".mypy_cache",
    ".cache",
    ".tox",
    "__pycache__",
];

pub(super) fn detect_large_untracked_dirs(
    files: &[PathBuf],
    dirs: &[PathBuf],
    threshold: Option<i64>,
) -> Vec<LargeUntrackedDir> {
    let Some(threshold) = threshold else {
        return Vec::new();
    };
    if threshold <= 0 {
        return Vec::new();
    }

    let mut counts: BTreeMap<PathBuf, i64> = BTreeMap::new();

    let mut sorted_dirs: Vec<&PathBuf> = dirs.iter().collect();
    sorted_dirs.sort_by(|a, b| {
        let a_components = a.components().count();
        let b_components = b.components().count();
        b_components.cmp(&a_components).then_with(|| a.cmp(b))
    });

    for file in files {
        let mut key: Option<PathBuf> = None;
        for dir in &sorted_dirs {
            if file.starts_with(dir.as_path()) {
                key = Some((*dir).clone());
                break;
            }
        }
        let key = key.unwrap_or_else(|| {
            file.parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        });
        let entry = counts.entry(key).or_insert(0);
        *entry += 1;
    }

    let mut result: Vec<LargeUntrackedDir> = counts
        .into_iter()
        .filter(|(_, count)| *count >= threshold)
        .map(|(path, file_count)| LargeUntrackedDir { path, file_count })
        .collect();
    result.sort_by(|a, b| {
        b.file_count
            .cmp(&a.file_count)
            .then_with(|| a.path.cmp(&b.path))
    });
    result
}

pub(super) fn should_ignore_for_snapshot(path: &Path) -> bool {
    path.components().any(|component| {
        if let Component::Normal(name) = component
            && let Some(name_str) = name.to_str()
        {
            return DEFAULT_IGNORED_DIR_NAMES
                .iter()
                .any(|ignored| ignored == &name_str);
        }
        false
    })
}

pub(super) fn is_force_included(path: &Path, force_include: &[PathBuf]) -> bool {
    force_include
        .iter()
        .any(|candidate| path.starts_with(candidate.as_path()))
}

pub(super) fn untracked_file_size(path: &Path) -> io::Result<Option<i64>> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(None);
    };

    let Ok(len_i64) = i64::try_from(metadata.len()) else {
        return Ok(Some(i64::MAX));
    };
    Ok(Some(len_i64))
}

/// Captures the working tree status under `repo_root`, optionally limited by `repo_prefix`.
/// Returns the result as a `StatusSnapshot`.
pub(super) fn capture_status_snapshot(
    repo_root: &Path,
    repo_prefix: Option<&Path>,
    ignore_large_untracked_files: Option<i64>,
    ignore_large_untracked_dirs: Option<i64>,
    force_include: &[PathBuf],
) -> Result<StatusSnapshot, GitToolingError> {
    // Ask git for the zero-delimited porcelain status so we can enumerate
    // tracked, untracked, and ignored entries (including ones filtered by prefix).
    // This keeps the snapshot consistent without multiple git invocations.
    let mut args = vec![
        OsString::from("status"),
        OsString::from("--porcelain=2"),
        OsString::from("-z"),
        OsString::from("--untracked-files=all"),
    ];
    if let Some(prefix) = repo_prefix {
        args.push(OsString::from("--"));
        args.push(prefix.as_os_str().to_os_string());
    }

    let output = run_git_for_stdout_all(repo_root, args, /*env*/ None)?;
    if output.is_empty() {
        return Ok(StatusSnapshot::default());
    }

    let mut snapshot = StatusSnapshot::default();
    let mut untracked_files_for_dir_scan: Vec<PathBuf> = Vec::new();
    let mut expect_rename_source = false;
    for entry in output.split('\0') {
        if entry.is_empty() {
            continue;
        }
        if expect_rename_source {
            let normalized = normalize_relative_path(Path::new(entry))?;
            snapshot.tracked_paths.push(normalized);
            expect_rename_source = false;
            continue;
        }

        let record_type = entry.as_bytes().first().copied().unwrap_or(b' ');
        match record_type {
            b'?' | b'!' => {
                let mut parts = entry.splitn(2, ' ');
                let code = parts.next();
                let path_part = parts.next();
                let (Some(code), Some(path_part)) = (code, path_part) else {
                    continue;
                };
                if path_part.is_empty() {
                    continue;
                }

                let normalized = normalize_relative_path(Path::new(path_part))?;
                if should_ignore_for_snapshot(&normalized) {
                    continue;
                }
                let absolute = repo_root.join(&normalized);
                let is_dir = absolute.is_dir();
                if is_dir {
                    snapshot.untracked.dirs.push(normalized);
                } else if code == "?" {
                    untracked_files_for_dir_scan.push(normalized.clone());
                    if let Some(threshold) = ignore_large_untracked_files
                        && threshold > 0
                        && !is_force_included(&normalized, force_include)
                        && let Ok(Some(byte_size)) = untracked_file_size(&absolute)
                        && byte_size > threshold
                    {
                        snapshot
                            .untracked
                            .ignored_untracked_files
                            .push(IgnoredUntrackedFile {
                                path: normalized,
                                byte_size,
                            });
                    } else {
                        snapshot.untracked.files.push(normalized.clone());
                        snapshot
                            .untracked
                            .untracked_files_for_index
                            .push(normalized);
                    }
                } else {
                    snapshot.untracked.files.push(normalized);
                }
            }
            b'1' => {
                if let Some(path) =
                    extract_status_path_after_fields(entry, /*fields_before_path*/ 8)
                {
                    let normalized = normalize_relative_path(Path::new(path))?;
                    snapshot.tracked_paths.push(normalized);
                }
            }
            b'2' => {
                if let Some(path) =
                    extract_status_path_after_fields(entry, /*fields_before_path*/ 9)
                {
                    let normalized = normalize_relative_path(Path::new(path))?;
                    snapshot.tracked_paths.push(normalized);
                }
                expect_rename_source = true;
            }
            b'u' => {
                if let Some(path) =
                    extract_status_path_after_fields(entry, /*fields_before_path*/ 10)
                {
                    let normalized = normalize_relative_path(Path::new(path))?;
                    snapshot.tracked_paths.push(normalized);
                }
            }
            _ => {}
        }
    }

    if let Some(threshold) = ignore_large_untracked_dirs
        && threshold > 0
    {
        let ignored_large_untracked_dirs = detect_large_untracked_dirs(
            &untracked_files_for_dir_scan,
            &snapshot.untracked.dirs,
            Some(threshold),
        )
        .into_iter()
        .filter(|entry| !entry.path.as_os_str().is_empty() && entry.path != Path::new("."))
        .collect::<Vec<_>>();

        if !ignored_large_untracked_dirs.is_empty() {
            let ignored_dir_paths = ignored_large_untracked_dirs
                .iter()
                .map(|entry| entry.path.as_path())
                .collect::<Vec<_>>();

            snapshot
                .untracked
                .files
                .retain(|path| !ignored_dir_paths.iter().any(|dir| path.starts_with(dir)));
            snapshot
                .untracked
                .dirs
                .retain(|path| !ignored_dir_paths.iter().any(|dir| path.starts_with(dir)));
            snapshot
                .untracked
                .untracked_files_for_index
                .retain(|path| !ignored_dir_paths.iter().any(|dir| path.starts_with(dir)));
            snapshot.untracked.ignored_untracked_files.retain(|file| {
                !ignored_dir_paths
                    .iter()
                    .any(|dir| file.path.starts_with(dir))
            });

            snapshot.untracked.ignored_large_untracked_dir_files = untracked_files_for_dir_scan
                .into_iter()
                .filter(|path| ignored_dir_paths.iter().any(|dir| path.starts_with(dir)))
                .collect();
            snapshot.untracked.ignored_large_untracked_dirs = ignored_large_untracked_dirs;
        }
    }

    Ok(snapshot)
}

/// Captures the untracked and ignored entries under `repo_root`, optionally limited by `repo_prefix`.
/// Returns the result as an `UntrackedSnapshot`.
pub(super) fn capture_existing_untracked(
    repo_root: &Path,
    repo_prefix: Option<&Path>,
    ignore_large_untracked_files: Option<i64>,
    ignore_large_untracked_dirs: Option<i64>,
    force_include: &[PathBuf],
) -> Result<UntrackedSnapshot, GitToolingError> {
    Ok(capture_status_snapshot(
        repo_root,
        repo_prefix,
        ignore_large_untracked_files,
        ignore_large_untracked_dirs,
        force_include,
    )?
    .untracked)
}

fn extract_status_path_after_fields(record: &str, fields_before_path: i64) -> Option<&str> {
    if fields_before_path <= 0 {
        return None;
    }
    let mut spaces = 0_i64;
    for (idx, byte) in record.as_bytes().iter().enumerate() {
        if *byte == b' ' {
            spaces += 1;
            if spaces == fields_before_path {
                return record.get((idx + 1)..).filter(|path| !path.is_empty());
            }
        }
    }
    None
}
