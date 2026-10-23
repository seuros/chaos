use super::*;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

#[test]
fn resolve_executable_fd_uses_supplied_search_path() {
    let tempdir = TempDir::new().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let program = tempdir.path().join("hello");
    std::fs::write(&program, b"#!/bin/sh\nexit 0\n")
        .unwrap_or_else(|err| panic!("write executable: {err}"));
    let mut permissions = std::fs::metadata(&program)
        .unwrap_or_else(|err| panic!("metadata: {err}"))
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&program, permissions)
        .unwrap_or_else(|err| panic!("chmod executable: {err}"));

    let fd = resolve_executable_fd_with_search_path("hello", Some(tempdir.path().as_os_str()))
        .unwrap_or_else(|err| panic!("resolve executable: {err}"));

    drop(fd);
}
