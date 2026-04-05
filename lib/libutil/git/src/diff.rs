use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use similar::TextDiff;

use crate::error::GitError;
use crate::ext::GitResultExt;
use crate::open_repo;
use crate::status;

/// Generate a unified diff.
///
/// - `base`: ref to diff against (default: HEAD)
/// - `paths`: optional path filters
pub fn diff(cwd: &Path, base: Option<&str>, paths: Option<&[&str]>) -> Result<String, GitError> {
    if base.is_none_or(|spec| spec == "HEAD") {
        return diff_against_worktree(cwd, paths);
    }

    let repo = open_repo(cwd)?;

    let Some(base_spec) = base else {
        return diff_against_worktree(cwd, paths);
    };
    let base_obj = repo
        .rev_parse_single(base_spec)
        .map_err(|e| GitError::RefNotFound(format!("{base_spec}: {e}")))?;
    let base_tree = base_obj.object().git_op()?.peel_to_tree().git_op()?;

    let head_tree = repo
        .head_id()
        .git_op()?
        .object()
        .git_op()?
        .peel_to_tree()
        .git_op()?;

    // Use diff_tree_to_tree which returns Vec<ChangeDetached>
    let changes = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)
        .git_op()?;

    let mut out = String::new();

    use gix::object::tree::diff::ChangeDetached;
    for change in changes {
        match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                id,
                ..
            } => {
                let path = location.to_string();
                if !matches_filter(&path, paths) {
                    continue;
                }
                out.push_str(&format!("--- /dev/null\n+++ b/{path}\n"));
                if entry_mode.is_blob()
                    && let Ok(obj) = repo.find_object(id)
                    && let Ok(s) = std::str::from_utf8(&obj.data)
                {
                    for line in s.lines() {
                        out.push_str(&format!("+{line}\n"));
                    }
                }
            }
            ChangeDetached::Deletion {
                location,
                entry_mode,
                id,
                ..
            } => {
                let path = location.to_string();
                if !matches_filter(&path, paths) {
                    continue;
                }
                out.push_str(&format!("--- a/{path}\n+++ /dev/null\n"));
                if entry_mode.is_blob()
                    && let Ok(obj) = repo.find_object(id)
                    && let Ok(s) = std::str::from_utf8(&obj.data)
                {
                    for line in s.lines() {
                        out.push_str(&format!("-{line}\n"));
                    }
                }
            }
            ChangeDetached::Modification {
                location,
                previous_id,
                id,
                ..
            } => {
                let path = location.to_string();
                if !matches_filter(&path, paths) {
                    continue;
                }
                out.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
                if let (Ok(old_obj), Ok(new_obj)) =
                    (repo.find_object(previous_id), repo.find_object(id))
                    && let (Ok(old_s), Ok(new_s)) = (
                        std::str::from_utf8(&old_obj.data),
                        std::str::from_utf8(&new_obj.data),
                    )
                {
                    for line in old_s.lines() {
                        out.push_str(&format!("-{line}\n"));
                    }
                    for line in new_s.lines() {
                        out.push_str(&format!("+{line}\n"));
                    }
                }
            }
            ChangeDetached::Rewrite {
                source_location,
                location,
                ..
            } => {
                let src = source_location.to_string();
                let dst = location.to_string();
                if !matches_filter(&src, paths) && !matches_filter(&dst, paths) {
                    continue;
                }
                out.push_str(&format!("rename from {src}\nrename to {dst}\n"));
            }
        }
    }

    Ok(out)
}

fn diff_against_worktree(cwd: &Path, paths: Option<&[&str]>) -> Result<String, GitError> {
    let repo = open_repo(cwd)?;
    let root = repo
        .workdir()
        .ok_or_else(|| GitError::Operation("repository has no worktree".to_string()))?;
    let head_tree = repo
        .head_id()
        .git_op()?
        .object()
        .git_op()?
        .peel_to_tree()
        .git_op()?;

    let status = status::collect(cwd)?;
    let mut changed_paths = BTreeSet::new();
    for item in status.staged.into_iter().chain(status.unstaged) {
        if matches_filter(&item.path, paths) {
            changed_paths.insert(item.path);
        }
    }

    let mut out = String::new();
    for path in changed_paths {
        let old_content = tree_blob_content(&head_tree, &path)?;
        let new_content = worktree_blob_content(root, &path)?;
        if old_content == new_content {
            continue;
        }
        render_unified_diff(&mut out, &path, old_content.as_deref(), new_content.as_deref());
    }

    Ok(out)
}

fn tree_blob_content(tree: &gix::Tree<'_>, path: &str) -> Result<Option<String>, GitError> {
    let Some(entry) = tree.lookup_entry_by_path(path).git_op()? else {
        return Ok(None);
    };
    let object = entry.object().git_op()?;
    Ok(Some(String::from_utf8_lossy(&object.data).into_owned()))
}

fn worktree_blob_content(root: &Path, path: &str) -> Result<Option<String>, GitError> {
    let full_path = root.join(path);
    match fs::read(&full_path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(GitError::Operation(format!(
            "failed to read worktree file {path}: {err}"
        ))),
    }
}

fn render_unified_diff(out: &mut String, path: &str, old: Option<&str>, new: Option<&str>) {
    match (old, new) {
        (None, Some(new_text)) => {
            out.push_str(&format!("--- /dev/null\n+++ b/{path}\n"));
            for change in TextDiff::from_lines("", new_text).iter_all_changes() {
                if change.tag() == similar::ChangeTag::Insert {
                    out.push('+');
                    out.push_str(change.value());
                }
            }
        }
        (Some(old_text), None) => {
            out.push_str(&format!("--- a/{path}\n+++ /dev/null\n"));
            for change in TextDiff::from_lines(old_text, "").iter_all_changes() {
                if change.tag() == similar::ChangeTag::Delete {
                    out.push('-');
                    out.push_str(change.value());
                }
            }
        }
        (Some(old_text), Some(new_text)) => {
            out.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
            for change in TextDiff::from_lines(old_text, new_text).iter_all_changes() {
                match change.tag() {
                    similar::ChangeTag::Delete => {
                        out.push('-');
                        out.push_str(change.value());
                    }
                    similar::ChangeTag::Insert => {
                        out.push('+');
                        out.push_str(change.value());
                    }
                    similar::ChangeTag::Equal => {}
                }
            }
        }
        (None, None) => {}
    }
}

fn matches_filter(path: &str, paths: Option<&[&str]>) -> bool {
    match paths {
        None => true,
        Some(filters) => filters.iter().any(|f| path.starts_with(f)),
    }
}
