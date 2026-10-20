//! Single-file buffer updates using the same native parser and hunk algorithm.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    ApplyPatchAction, ApplyPatchError, ApplyPatchFileChange, Hunk, derive_new_contents_from_text,
    parse_patch,
};

fn single_update(patch: &str) -> Result<Hunk, ApplyPatchError> {
    let mut parsed = parse_patch(patch)?;
    if parsed.hunks.len() != 1 {
        return Err(ApplyPatchError::ComputeReplacements(
            "editor patches support exactly one existing-file text update".into(),
        ));
    }
    match parsed.hunks.pop() {
        Some(
            hunk @ Hunk::UpdateFile {
                move_path: None, ..
            },
        ) => Ok(hunk),
        _ => Err(ApplyPatchError::ComputeReplacements(
            "editor patches do not support create, delete, rename or multi-file actions".into(),
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
mod tests {
    use super::*;

    #[test]
    fn editor_hunks_match_disk_computation_without_effects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.txt");
        std::fs::write(&path, "before\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: buffer.txt\n-before\n+after\n*** End Patch";
        let action = prepare_text_update(patch, dir.path(), "before\n".into()).unwrap();
        let disk = crate::maybe_parse_apply_patch_verified(
            &["apply_patch".into(), patch.into()],
            dir.path(),
        );
        assert_eq!(crate::MaybeApplyPatchVerified::Body(action), disk);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "before\n");
    }

    #[test]
    fn editor_accepts_unsaved_content_without_a_disk_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let patch =
            "*** Begin Patch\n*** Update File: not-on-disk.txt\n-unsaved\n+updated\n*** End Patch";
        let action = prepare_text_update(patch, dir.path(), "unsaved\n".into()).unwrap();
        assert!(matches!(action.changes().values().next(),
            Some(ApplyPatchFileChange::Update { new_content, .. }) if new_content == "updated\n"));
        assert!(!dir.path().join("not-on-disk.txt").exists());
        assert!(prepare_text_update(patch, dir.path(), "different\n".into()).is_err());
    }
}
