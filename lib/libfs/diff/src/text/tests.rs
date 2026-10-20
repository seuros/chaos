use super::*;
use std::assert_matches;

#[test]
fn editor_creation_uses_native_add_parsing_without_disk_effects() {
    let dir = tempfile::tempdir().unwrap();
    let patch = "*** Begin Patch\n*** Add File: new.txt\n+created\n*** End Patch";
    assert_eq!(
        text_patch_path(patch, dir.path()).unwrap(),
        (dir.path().join("new.txt"), true)
    );
    let action = prepare_text_patch(patch, dir.path(), None).unwrap();
    assert_matches!(action.changes().values().next(),
        Some(ApplyPatchFileChange::Add { content }) if content == "created\n");
    assert!(!dir.path().join("new.txt").exists());
    assert!(prepare_text_patch(patch, dir.path(), Some(String::new())).is_err());
    assert!(text_update_path(patch, dir.path()).is_err());
}

#[test]
fn editor_hunks_match_disk_computation_without_effects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("buffer.txt");
    std::fs::write(&path, "before\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: buffer.txt\n-before\n+after\n*** End Patch";
    let action = prepare_text_update(patch, dir.path(), "before\n".into()).unwrap();
    let disk =
        crate::maybe_parse_apply_patch_verified(&["apply_patch".into(), patch.into()], dir.path());
    assert_eq!(crate::MaybeApplyPatchVerified::Body(action), disk);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "before\n");
}

#[test]
fn editor_accepts_unsaved_content_without_a_disk_mirror() {
    let dir = tempfile::tempdir().unwrap();
    let patch =
        "*** Begin Patch\n*** Update File: not-on-disk.txt\n-unsaved\n+updated\n*** End Patch";
    let action = prepare_text_update(patch, dir.path(), "unsaved\n".into()).unwrap();
    assert_matches!(action.changes().values().next(),
        Some(ApplyPatchFileChange::Update { new_content, .. }) if new_content == "updated\n");
    assert!(!dir.path().join("not-on-disk.txt").exists());
    assert!(prepare_text_update(patch, dir.path(), "different\n".into()).is_err());
}
