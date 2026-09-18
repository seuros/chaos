use super::*;

use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

#[test]
fn validate_existing_ca_key_file_rejects_group_world_permissions() {
    let dir = tempdir().unwrap();
    let key_path = dir.path().join("ca.key");
    fs::write(&key_path, "key").unwrap();
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();

    let err = validate_existing_ca_key_file(&key_path).unwrap_err();
    assert!(
        err.to_string().contains("group/world accessible"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn validate_existing_ca_key_file_rejects_symlink() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let target = dir.path().join("real.key");
    let link = dir.path().join("ca.key");
    fs::write(&target, "key").unwrap();
    symlink(&target, &link).unwrap();

    let err = validate_existing_ca_key_file(&link).unwrap_err();
    assert!(
        err.to_string().contains("symlink"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn validate_existing_ca_key_file_allows_private_permissions() {
    let dir = tempdir().unwrap();
    let key_path = dir.path().join("ca.key");
    fs::write(&key_path, "key").unwrap();
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();

    validate_existing_ca_key_file(&key_path).unwrap();
}
