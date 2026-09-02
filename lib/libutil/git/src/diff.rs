use std::collections::BTreeSet;
use std::fs;
use std::ops::ControlFlow;
use std::path::Path;

use gix::bstr::ByteSlice;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use similar::ChangeTag;
use similar::TextDiff;

use crate::error::GitError;
use crate::ext::GitResultExt;
use crate::open_repo;
use crate::status;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffScope {
    Worktree,
    Staged,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffStatus {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffFile {
    pub path: String,
    pub status: DiffStatus,
    pub binary: bool,
    pub additions: Option<usize>,
    pub deletions: Option<usize>,
    pub patch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub binary_files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WhitespaceError {
    pub path: String,
    pub line: usize,
    pub kind: &'static str,
    pub message: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    pub files: Vec<DiffFile>,
    pub summary: DiffSummary,
    pub whitespace_errors: Vec<WhitespaceError>,
}

/// Compare repository content at the requested scope and return a structured report.
///
/// - `worktree`: index to working tree (unstaged tracked changes)
/// - `staged`: base tree to index
/// - `all`: base tree to working tree (staged and unstaged tracked changes)
pub fn diff_report(
    cwd: &Path,
    scope: DiffScope,
    base: Option<&str>,
    paths: Option<&[&str]>,
    check_whitespace: bool,
) -> Result<DiffReport, GitError> {
    if scope == DiffScope::Worktree && base.is_some() {
        return Err(GitError::InvalidInput(
            "base cannot be used with worktree scope; worktree compares the index to the working tree"
                .to_string(),
        ));
    }

    let repo = open_repo(cwd)?;
    let root = repo
        .workdir()
        .ok_or_else(|| GitError::Operation("repository has no worktree".to_string()))?;
    let index = repo
        .index_or_load_from_head_or_empty()
        .map_err(|e| GitError::Operation(e.to_string()))?
        .into_owned();
    let repository_status = status::collect(cwd)?;

    let base_tree = match scope {
        DiffScope::Worktree => None,
        DiffScope::Staged | DiffScope::All => Some(resolve_base_tree(&repo, base)?),
    };

    let mut changed_paths = BTreeSet::new();
    if scope != DiffScope::Worktree {
        collect_tree_index_paths(
            &repo,
            &index,
            base_tree
                .as_ref()
                .expect("base tree for staged or all scope"),
            paths,
            &mut changed_paths,
        )?;
    }
    if scope != DiffScope::Staged {
        extend_filtered_paths(
            &mut changed_paths,
            repository_status.unstaged.into_iter().map(|item| item.path),
            paths,
        );
    }

    let mut files = Vec::new();
    let mut whitespace_errors = Vec::new();
    for path in changed_paths {
        let old_content = match scope {
            DiffScope::Worktree => index_blob_content(&repo, &index, &path)?,
            DiffScope::Staged | DiffScope::All => tree_blob_content(
                base_tree
                    .as_ref()
                    .expect("base tree for staged or all scope"),
                &path,
            )?,
        };
        let new_content = match scope {
            DiffScope::Staged => index_blob_content(&repo, &index, &path)?,
            DiffScope::Worktree | DiffScope::All => worktree_blob_content(root, &path)?,
        };

        if old_content == new_content {
            continue;
        }

        let (file, mut file_errors) =
            build_diff_file(path, old_content, new_content, check_whitespace);
        files.push(file);
        whitespace_errors.append(&mut file_errors);
    }

    let summary = DiffSummary {
        files_changed: files.len(),
        insertions: files.iter().filter_map(|file| file.additions).sum(),
        deletions: files.iter().filter_map(|file| file.deletions).sum(),
        binary_files: files.iter().filter(|file| file.binary).count(),
    };

    Ok(DiffReport {
        files,
        summary,
        whitespace_errors,
    })
}

/// Generate a unified diff from a base tree to the working tree.
pub fn diff(cwd: &Path, base: Option<&str>, paths: Option<&[&str]>) -> Result<String, GitError> {
    let report = diff_report(cwd, DiffScope::All, base, paths, false)?;
    Ok(report.files.into_iter().map(|file| file.patch).collect())
}

fn resolve_base_tree<'repo>(
    repo: &'repo gix::Repository,
    base: Option<&str>,
) -> Result<gix::Tree<'repo>, GitError> {
    let base_spec = base.unwrap_or("HEAD");
    if base_spec == "HEAD" {
        return match repo.head_id() {
            Ok(id) => id.object().git_op()?.peel_to_tree().git_op(),
            Err(_) => Ok(repo.empty_tree()),
        };
    }

    repo.rev_parse_single(base_spec)
        .map_err(|e| GitError::RefNotFound(format!("{base_spec}: {e}")))?
        .object()
        .git_op()?
        .peel_to_tree()
        .git_op()
}

fn collect_tree_index_paths(
    repo: &gix::Repository,
    index: &gix::index::File,
    tree: &gix::Tree<'_>,
    paths: Option<&[&str]>,
    changed_paths: &mut BTreeSet<String>,
) -> Result<(), GitError> {
    repo.tree_index_status(
        tree.id.as_ref(),
        index,
        None,
        gix::status::tree_index::TrackRenames::Disabled,
        |change, _, _| {
            use gix::diff::index::ChangeRef;

            let path = match change {
                ChangeRef::Addition { location, .. }
                | ChangeRef::Deletion { location, .. }
                | ChangeRef::Modification { location, .. }
                | ChangeRef::Rewrite { location, .. } => location.to_string(),
            };
            if matches_filter(&path, paths) {
                changed_paths.insert(path);
            }
            Ok::<_, std::convert::Infallible>(ControlFlow::Continue(()))
        },
    )
    .git_op()?;
    Ok(())
}

fn extend_filtered_paths(
    changed_paths: &mut BTreeSet<String>,
    candidates: impl IntoIterator<Item = String>,
    paths: Option<&[&str]>,
) {
    changed_paths.extend(
        candidates
            .into_iter()
            .filter(|path| matches_filter(path, paths)),
    );
}

fn tree_blob_content(tree: &gix::Tree<'_>, path: &str) -> Result<Option<Vec<u8>>, GitError> {
    let Some(entry) = tree.lookup_entry_by_path(path).git_op()? else {
        return Ok(None);
    };
    let object = entry.object().git_op()?;
    Ok(Some(object.data.to_vec()))
}

fn index_blob_content(
    repo: &gix::Repository,
    index: &gix::index::File,
    path: &str,
) -> Result<Option<Vec<u8>>, GitError> {
    let Some(range) = index.entry_range(path.as_bytes().as_bstr()) else {
        return Ok(None);
    };
    let entry = index.entries()[range]
        .iter()
        .find(|entry| entry.stage() == gix::index::entry::Stage::Unconflicted)
        .ok_or_else(|| GitError::Conflict(path.to_string()))?;
    let object = repo.find_object(entry.id).git_op()?;
    Ok(Some(object.data.to_vec()))
}

fn worktree_blob_content(root: &Path, path: &str) -> Result<Option<Vec<u8>>, GitError> {
    let full_path = root.join(path);
    let metadata = match fs::symlink_metadata(&full_path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(GitError::Operation(format!(
                "failed to inspect worktree file {path}: {err}"
            )));
        }
    };
    if metadata.file_type().is_symlink() {
        return fs::read_link(&full_path)
            .map(|target| Some(target.to_string_lossy().into_owned().into_bytes()))
            .map_err(|err| {
                GitError::Operation(format!("failed to read worktree symlink {path}: {err}"))
            });
    }
    if !metadata.is_file() {
        return Ok(None);
    }
    fs::read(&full_path)
        .map(Some)
        .map_err(|err| GitError::Operation(format!("failed to read worktree file {path}: {err}")))
}

fn build_diff_file(
    path: String,
    old: Option<Vec<u8>>,
    new: Option<Vec<u8>>,
    check_whitespace: bool,
) -> (DiffFile, Vec<WhitespaceError>) {
    let status = match (old.is_some(), new.is_some()) {
        (false, true) => DiffStatus::Added,
        (true, false) => DiffStatus::Deleted,
        (true, true) => DiffStatus::Modified,
        (false, false) => unreachable!("unchanged missing file was filtered"),
    };
    let binary = old.as_deref().is_some_and(is_binary) || new.as_deref().is_some_and(is_binary);
    let old_label = if old.is_some() {
        format!("a/{path}")
    } else {
        "/dev/null".to_string()
    };
    let new_label = if new.is_some() {
        format!("b/{path}")
    } else {
        "/dev/null".to_string()
    };

    let mut patch = format!("diff --git a/{path} b/{path}\n");
    if binary {
        patch.push_str(&format!(
            "Binary files {old_label} and {new_label} differ\n"
        ));
        return (
            DiffFile {
                path,
                status,
                binary,
                additions: None,
                deletions: None,
                patch,
            },
            Vec::new(),
        );
    }

    let old_text = String::from_utf8_lossy(old.as_deref().unwrap_or_default());
    let new_text = String::from_utf8_lossy(new.as_deref().unwrap_or_default());
    let text_diff = TextDiff::from_lines(old_text.as_ref(), new_text.as_ref());
    let mut additions = 0;
    let mut deletions = 0;
    for change in text_diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    patch.push_str(
        &text_diff
            .unified_diff()
            .context_radius(3)
            .header(&old_label, &new_label)
            .to_string(),
    );
    let whitespace_errors = if check_whitespace {
        collect_whitespace_errors(&path, &text_diff)
    } else {
        Vec::new()
    };

    (
        DiffFile {
            path,
            status,
            binary,
            additions: Some(additions),
            deletions: Some(deletions),
            patch,
        },
        whitespace_errors,
    )
}

fn collect_whitespace_errors(path: &str, diff: &TextDiff<'_, '_, str>) -> Vec<WhitespaceError> {
    let mut errors = Vec::new();
    let mut new_line = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => continue,
            ChangeTag::Equal => {
                new_line += 1;
                continue;
            }
            ChangeTag::Insert => new_line += 1,
        }

        let line = change.value().strip_suffix('\n').unwrap_or(change.value());
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut report = |kind, message| {
            errors.push(WhitespaceError {
                path: path.to_string(),
                line: new_line,
                kind,
                message,
            });
        };
        if line.ends_with([' ', '\t']) {
            report("trailing_whitespace", "new line has trailing whitespace");
        }
        if has_space_before_tab_in_indent(line) {
            report(
                "space_before_tab",
                "new line has a space before a tab in its indentation",
            );
        }
        if is_conflict_marker(line) {
            report("conflict_marker", "new line introduces a conflict marker");
        }
    }
    errors
}

fn has_space_before_tab_in_indent(line: &str) -> bool {
    let mut saw_space = false;
    for byte in line.bytes() {
        match byte {
            b' ' => saw_space = true,
            b'\t' if saw_space => return true,
            b'\t' => {}
            _ => break,
        }
    }
    false
}

fn is_conflict_marker(line: &str) -> bool {
    ["<<<<<<<", "=======", ">>>>>>>"]
        .iter()
        .any(|marker| line.starts_with(marker))
}

fn is_binary(content: &[u8]) -> bool {
    content[..content.len().min(8192)].contains(&0)
}

fn matches_filter(path: &str, paths: Option<&[&str]>) -> bool {
    match paths {
        None => true,
        Some(filters) => filters.iter().any(|filter| {
            let filter = filter.trim_end_matches('/');
            path == filter
                || path
                    .strip_prefix(filter)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        }),
    }
}
