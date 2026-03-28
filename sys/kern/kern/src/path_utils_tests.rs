use std::path::PathBuf;

use pretty_assertions::assert_eq;

use super::normalize_for_path_comparison;

#[test]
fn canonical_path_is_returned() {
    let cwd = std::env::current_dir().expect("cwd");
    let normalized = normalize_for_path_comparison(&cwd).expect("canonicalize");
    assert_eq!(normalized, cwd.canonicalize().expect("canonicalize"));
}
