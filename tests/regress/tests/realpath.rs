//! Public-API tests for `chaos-realpath` — absolute paths, no exceptions.
//!
//! `AbsolutePathBuf` is a type-system promise that a path has already
//! been resolved, normalized, and expanded. These tests pin the three
//! doors into that guarantee: direct construction with/without a base,
//! tilde expansion, and deserialization through the thread-local guard
//! that lets serde carry a base path into a trait impl it can't
//! otherwise see.

use chaos_realpath::AbsolutePathBuf;
use chaos_realpath::AbsolutePathBufGuard;
use dirs::home_dir;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[test]
fn resolve_path_against_base_handles_absolute_and_relative_inputs() {
    let base_dir = tempdir().expect("base dir");
    let absolute_dir = tempdir().expect("absolute dir");

    // Absolute input wins: base is ignored.
    let abs_input = absolute_dir.path().join("file.txt");
    let resolved =
        AbsolutePathBuf::resolve_path_against_base(&abs_input, base_dir.path()).expect("absolute");
    assert_eq!(resolved.as_path(), abs_input.as_path());

    // Relative input gets joined onto the base.
    let resolved =
        AbsolutePathBuf::resolve_path_against_base("file.txt", base_dir.path()).expect("relative");
    assert_eq!(
        resolved.as_path(),
        base_dir.path().join("file.txt").as_path()
    );
}

#[test]
fn deserialization_guard_carries_base_path_and_expands_tilde() {
    let temp_dir = tempdir().expect("base dir");
    let base = temp_dir.path();

    // Relative path inside the guard resolves against the base.
    let resolved = {
        let _guard = AbsolutePathBufGuard::new(base);
        serde_json::from_str::<AbsolutePathBuf>(r#""subdir/file.txt""#).expect("deserialize")
    };
    assert_eq!(resolved.as_path(), base.join("subdir/file.txt").as_path());

    // Tilde expansion: bare "~", "~/code", and "~//code" all land on
    // $HOME / $HOME/code. Skip the trio if this environment has no
    // home directory (CI sandbox edge case).
    let Some(home) = home_dir() else {
        return;
    };

    let guard = AbsolutePathBufGuard::new(base);
    let tilde_only = serde_json::from_str::<AbsolutePathBuf>(r#""~""#).expect("deserialize ~");
    assert_eq!(tilde_only.as_path(), home.as_path());

    let tilde_sub =
        serde_json::from_str::<AbsolutePathBuf>(r#""~/code""#).expect("deserialize ~/code");
    assert_eq!(tilde_sub.as_path(), home.join("code").as_path());

    let tilde_doubleslash =
        serde_json::from_str::<AbsolutePathBuf>(r#""~//code""#).expect("deserialize ~//code");
    assert_eq!(tilde_doubleslash.as_path(), home.join("code").as_path());
    drop(guard);
}
