use std::collections::BTreeSet;
use std::path::Component;
use std::path::Path;

use gix::bstr::BString;
use gix::bstr::ByteSlice;
use serde::Serialize;

use crate::error::GitError;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct AddResult {
    pub staged: Vec<String>,
    pub removed: Vec<String>,
}

pub fn add(cwd: &Path, paths: &[String]) -> Result<AddResult, GitError> {
    if paths.is_empty() {
        return Err(GitError::InvalidInput(
            "at least one repository-relative file path is required".to_string(),
        ));
    }

    let repo = open_repo(cwd)?;
    if repo.is_bare() {
        return Err(GitError::Unsupported(
            "git_add requires a non-bare repository".to_string(),
        ));
    }

    let mut index = repo
        .index_or_load_from_head_or_empty()
        .map_err(|e| GitError::Operation(e.to_string()))?
        .into_owned();
    let (mut pipeline, _) = repo
        .filter_pipeline(None)
        .map_err(|e| GitError::Operation(e.to_string()))?;
    let mut excludes = repo
        .excludes(
            &index,
            None,
            gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
        )
        .map_err(|e| GitError::Operation(e.to_string()))?;

    let mut normalized_paths = Vec::with_capacity(paths.len());
    let mut seen = BTreeSet::new();
    for path in paths {
        let normalized = normalize_explicit_path(path)?;
        if seen.insert(normalized.clone()) {
            normalized_paths.push(normalized);
        }
    }

    let mut staged = Vec::new();
    let mut removed = Vec::new();

    for path in normalized_paths {
        let display_path = path.to_string();
        let range = index.entry_range(path.as_bstr());
        let tracked = range.is_some();

        if let Some(range) = range {
            let entries = &index.entries()[range];
            if entries
                .iter()
                .any(|entry| entry.stage() != gix::index::entry::Stage::Unconflicted)
            {
                return Err(GitError::Conflict(display_path));
            }
            if entries
                .iter()
                .any(|entry| entry.mode == gix::index::entry::Mode::COMMIT)
            {
                return Err(GitError::Unsupported(format!(
                    "submodule staging is not supported: {display_path}"
                )));
            }
        }

        let worktree_path = repo
            .workdir_path(path.as_bstr())
            .ok_or_else(|| GitError::PathNotFound(display_path.clone()))?;
        let metadata = match gix::index::fs::Metadata::from_path_no_follow(&worktree_path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if !tracked {
                    return Err(GitError::PathNotFound(display_path));
                }
                index.remove_entries(|_, entry_path, _| entry_path == path.as_bstr());
                removed.push(display_path);
                continue;
            }
            Err(err) => return Err(GitError::Operation(err.to_string())),
        };

        if metadata.is_dir() {
            return Err(GitError::Unsupported(format!(
                "directory staging is not supported; pass files explicitly: {display_path}"
            )));
        }
        if !metadata.is_file() && !metadata.is_symlink() {
            return Err(GitError::Unsupported(format!(
                "unsupported worktree entry type: {display_path}"
            )));
        }

        let mode = if metadata.is_symlink() {
            gix::index::entry::Mode::SYMLINK
        } else if metadata.is_executable() {
            gix::index::entry::Mode::FILE_EXECUTABLE
        } else {
            gix::index::entry::Mode::FILE
        };

        if !tracked
            && excludes
                .at_entry(path.as_bstr(), Some(mode))
                .map_err(|e| GitError::Operation(e.to_string()))?
                .is_excluded()
        {
            return Err(GitError::IgnoredPath(display_path));
        }

        let Some((id, kind, _)) = pipeline
            .worktree_file_to_object(path.as_bstr(), &index)
            .map_err(|e| GitError::Operation(e.to_string()))?
        else {
            return Err(GitError::Unsupported(format!(
                "unable to stage worktree entry: {display_path}"
            )));
        };
        if kind == gix::objs::tree::EntryKind::Commit {
            return Err(GitError::Unsupported(format!(
                "submodule staging is not supported: {display_path}"
            )));
        }

        let stat = gix::index::entry::Stat::from_fs(&metadata)
            .map_err(|e| GitError::Operation(e.to_string()))?;
        if let Some(entry) = index
            .entry_mut_by_path_and_stage(path.as_bstr(), gix::index::entry::Stage::Unconflicted)
        {
            entry.stat = stat;
            entry.id = id;
            entry.flags =
                gix::index::entry::Flags::from_stage(gix::index::entry::Stage::Unconflicted);
            entry.mode = kind.into();
        } else {
            index.dangerously_push_entry(
                stat,
                id,
                gix::index::entry::Flags::from_stage(gix::index::entry::Stage::Unconflicted),
                kind.into(),
                path.as_bstr(),
            );
            index.sort_entries();
        }
        staged.push(display_path);
    }

    index.remove_tree();
    index
        .write(gix::index::write::Options::default())
        .map_err(|e| GitError::Operation(e.to_string()))?;

    Ok(AddResult { staged, removed })
}

fn normalize_explicit_path(raw: &str) -> Result<BString, GitError> {
    if raw.trim().is_empty() {
        return Err(GitError::InvalidInput(
            "git_add paths must not be empty".to_string(),
        ));
    }
    if raw.as_bytes().contains(&0) {
        return Err(GitError::InvalidInput(
            "git_add paths must not contain NUL bytes".to_string(),
        ));
    }

    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(GitError::InvalidInput(format!(
            "path must be repository-relative and may not escape the repository: {raw}"
        )));
    }

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => {
                components.push(component.to_str().ok_or_else(|| {
                    GitError::InvalidInput(format!("path is not valid UTF-8: {raw}"))
                })?);
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(GitError::InvalidInput(format!(
                    "path must be repository-relative and may not escape the repository: {raw}"
                )));
            }
        }
    }

    if components.is_empty() {
        return Err(GitError::InvalidInput(
            "repository root cannot be staged; pass files explicitly".to_string(),
        ));
    }
    if components[0].eq_ignore_ascii_case(".git") {
        return Err(GitError::InvalidInput(
            "paths inside the Git directory cannot be staged".to_string(),
        ));
    }
    Ok(components.join("/").into())
}
