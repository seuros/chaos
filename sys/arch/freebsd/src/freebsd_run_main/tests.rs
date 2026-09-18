use super::*;
use alcatraz_base::sandbox_policy::ResolveSandboxPoliciesError;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SocketPolicy;
use chaos_ipc::protocol::VfsPolicy;
use pretty_assertions::assert_eq;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

#[test]
fn resolve_sandbox_policies_derives_split_policies_from_sandbox_policy() {
    let sandbox_policy = SandboxPolicy::new_read_only_policy();

    let resolved =
        resolve_sandbox_policies(Path::new("/tmp"), Some(sandbox_policy.clone()), None, None)
            .expect("sandbox policy should resolve");

    assert_eq!(resolved.sandbox_policy, sandbox_policy);
    assert_eq!(resolved.vfs_policy, VfsPolicy::from(&sandbox_policy));
    assert_eq!(resolved.socket_policy, SocketPolicy::from(&sandbox_policy));
}

#[test]
fn resolve_sandbox_policies_derives_sandbox_policy_from_split_policies() {
    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let vfs_policy = VfsPolicy::from(&sandbox_policy);
    let socket_policy = SocketPolicy::from(&sandbox_policy);

    let resolved = resolve_sandbox_policies(
        Path::new("/tmp"),
        None,
        Some(vfs_policy.clone()),
        Some(socket_policy),
    )
    .expect("split policies should resolve");

    assert_eq!(resolved.sandbox_policy, sandbox_policy);
    assert_eq!(resolved.vfs_policy, vfs_policy);
    assert_eq!(resolved.socket_policy, socket_policy);
}

#[test]
fn resolve_sandbox_policies_rejects_partial_split_policies() {
    let err = resolve_sandbox_policies(
        Path::new("/tmp"),
        Some(SandboxPolicy::new_read_only_policy()),
        Some(VfsPolicy::default()),
        None,
    )
    .expect_err("partial split policies should fail");

    assert_eq!(err, ResolveSandboxPoliciesError::PartialSplitPolicies);
}

#[test]
fn resolve_sandbox_policies_rejects_mismatched_sandbox_and_split_inputs() {
    let err = resolve_sandbox_policies(
        Path::new("/tmp"),
        Some(SandboxPolicy::new_read_only_policy()),
        Some(VfsPolicy::unrestricted()),
        Some(SocketPolicy::Enabled),
    )
    .expect_err("mismatched sandbox and split policies should fail");
    assert!(
        matches!(
            err,
            ResolveSandboxPoliciesError::MismatchedSandboxPolicy { .. }
        ),
        "{err}"
    );
}

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
