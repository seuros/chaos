use std::path::Path;

use crate::error::GitError;
use crate::open_repo;

/// Generate a unified diff.
///
/// - `base`: ref to diff against (default: HEAD)
/// - `paths`: optional path filters
pub fn diff(cwd: &Path, base: Option<&str>, paths: Option<&[&str]>) -> Result<String, GitError> {
    let repo = open_repo(cwd)?;

    let base_spec = base.unwrap_or("HEAD");
    let base_obj = repo
        .rev_parse_single(base_spec)
        .map_err(|e| GitError::RefNotFound(format!("{base_spec}: {e}")))?;
    let base_tree = base_obj
        .object()
        .map_err(|e| GitError::Operation(e.to_string()))?
        .peel_to_tree()
        .map_err(|e| GitError::Operation(e.to_string()))?;

    let head_tree = repo
        .head_id()
        .map_err(|e| GitError::Operation(e.to_string()))?
        .object()
        .map_err(|e| GitError::Operation(e.to_string()))?
        .peel_to_tree()
        .map_err(|e| GitError::Operation(e.to_string()))?;

    // Use diff_tree_to_tree which returns Vec<ChangeDetached>
    let changes = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)
        .map_err(|e| GitError::Operation(e.to_string()))?;

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
                if entry_mode.is_blob() {
                    if let Ok(obj) = repo.find_object(id) {
                        if let Ok(s) = std::str::from_utf8(&obj.data) {
                            for line in s.lines() {
                                out.push_str(&format!("+{line}\n"));
                            }
                        }
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
                if entry_mode.is_blob() {
                    if let Ok(obj) = repo.find_object(id) {
                        if let Ok(s) = std::str::from_utf8(&obj.data) {
                            for line in s.lines() {
                                out.push_str(&format!("-{line}\n"));
                            }
                        }
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
                {
                    if let (Ok(old_s), Ok(new_s)) = (
                        std::str::from_utf8(&old_obj.data),
                        std::str::from_utf8(&new_obj.data),
                    ) {
                        for line in old_s.lines() {
                            out.push_str(&format!("-{line}\n"));
                        }
                        for line in new_s.lines() {
                            out.push_str(&format!("+{line}\n"));
                        }
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

fn matches_filter(path: &str, paths: Option<&[&str]>) -> bool {
    match paths {
        None => true,
        Some(filters) => filters.iter().any(|f| path.starts_with(f)),
    }
}
