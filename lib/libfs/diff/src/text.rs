//! Single-file editor patches using the native parser and hunk algorithm.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    ApplyPatchAction, ApplyPatchError, ApplyPatchFileChange, Hunk, derive_new_contents_from_text,
    parse_patch,
};

fn single_update(patch: &str) -> Result<Hunk, ApplyPatchError> {
    let hunk = single_patch(patch)?;
    if matches!(hunk, Hunk::UpdateFile { .. }) {
        Ok(hunk)
    } else {
        Err(ApplyPatchError::ComputeReplacements(
            "expected an existing-file text update".into(),
        ))
    }
}

fn single_patch(patch: &str) -> Result<Hunk, ApplyPatchError> {
    let mut parsed = parse_patch(patch)?;
    if parsed.hunks.len() != 1 {
        return Err(ApplyPatchError::ComputeReplacements(
            "editor patches support exactly one text update or file creation".into(),
        ));
    }
    match parsed.hunks.pop() {
        Some(
            hunk @ (Hunk::AddFile { .. }
            | Hunk::UpdateFile {
                move_path: None, ..
            }),
        ) => Ok(hunk),
        _ => Err(ApplyPatchError::ComputeReplacements(
            "editor patches do not support delete, rename or multi-file actions".into(),
        )),
    }
}

/// Validate the patch shape, returning its path and whether it creates a file.
pub fn text_patch_path(patch: &str, cwd: &Path) -> Result<(PathBuf, bool), ApplyPatchError> {
    let hunk = single_patch(patch)?;
    Ok((hunk.resolve_path(cwd), matches!(hunk, Hunk::AddFile { .. })))
}

/// Compute one editor creation or update, with no disk effects. Creation
/// requires confirmed absence; an existing editor buffer must not be replaced.
pub fn prepare_text_patch(
    patch: &str,
    cwd: &Path,
    original: Option<String>,
) -> Result<ApplyPatchAction, ApplyPatchError> {
    match (single_patch(patch)?, original) {
        (Hunk::AddFile { path, contents }, None) => Ok(ApplyPatchAction::from_parts(
            HashMap::from([(
                cwd.join(path),
                ApplyPatchFileChange::Add { content: contents },
            )]),
            cwd.to_path_buf(),
            patch.to_string(),
        )),
        (Hunk::UpdateFile { .. }, Some(original)) => prepare_text_update(patch, cwd, original),
        _ => Err(ApplyPatchError::ComputeReplacements(
            "editor patch conflicts with the current file state".into(),
        )),
    }
}

/// Validate the supported patch shape before requesting any editor text.
pub fn text_update_path(patch: &str, cwd: &Path) -> Result<PathBuf, ApplyPatchError> {
    Ok(single_update(patch)?.resolve_path(cwd))
}

/// Compute an existing-file patch from supplied text without reading or writing
/// disk. Formatting and hunk matching are shared with native disk updates.
pub fn prepare_text_update(
    patch: &str,
    cwd: &Path,
    original: String,
) -> Result<ApplyPatchAction, ApplyPatchError> {
    let Hunk::UpdateFile { path, chunks, .. } = single_update(patch)? else {
        unreachable!("single_update rejects all other variants")
    };
    let path = cwd.join(path);
    let applied = derive_new_contents_from_text(&path, &chunks, original)?;
    let unified_diff =
        similar::TextDiff::from_lines(&applied.original_contents, &applied.new_contents)
            .unified_diff()
            .context_radius(1)
            .to_string();
    Ok(ApplyPatchAction::from_parts(
        HashMap::from([(
            path,
            ApplyPatchFileChange::Update {
                unified_diff,
                move_path: None,
                new_content: applied.new_contents,
            },
        )]),
        cwd.to_path_buf(),
        patch.to_string(),
    ))
}

#[cfg(test)]
mod tests;
