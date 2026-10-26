#[cfg(test)]
use super::*;
#[cfg(test)]
use chaos_ipc::protocol::ReadOnlyAccess;
#[cfg(test)]
use chaos_ipc::protocol::SandboxPolicy;
#[cfg(test)]
use chaos_ipc::protocol::SocketPolicy;
#[cfg(test)]
use chaos_ipc::protocol::VfsPolicy;
#[cfg(test)]
use chaos_realpath::AbsolutePathBuf;
#[cfg(test)]
use pretty_assertions::assert_eq;

#[test]
fn resolve_sandbox_policies_accepts_split_policies_requiring_direct_runtime_enforcement() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let docs = temp_dir.path().join("docs");
    std::fs::create_dir_all(&docs).expect("create docs");
    let docs = AbsolutePathBuf::from_absolute_path(&docs).expect("absolute docs");
    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let vfs_policy = VfsPolicy::restricted(vec![
        chaos_ipc::permissions::VfsEntry {
            path: chaos_ipc::permissions::VfsPath::Special {
                value: chaos_ipc::permissions::VfsSpecialPath::Root,
            },
            access: chaos_ipc::permissions::VfsAccessMode::Read,
        },
        chaos_ipc::permissions::VfsEntry {
            path: chaos_ipc::permissions::VfsPath::Path { path: docs },
            access: chaos_ipc::permissions::VfsAccessMode::Write,
        },
    ]);

    let resolved = resolve_sandbox_policies(
        temp_dir.path(),
        Some(sandbox_policy.clone()),
        Some(vfs_policy.clone()),
        Some(SocketPolicy::Restricted),
    )
    .expect("split-only policy should preserve provided sandbox fallback");

    assert_eq!(resolved.sandbox_policy, sandbox_policy);
    assert_eq!(resolved.vfs_policy, vfs_policy);
    assert_eq!(resolved.socket_policy, SocketPolicy::Restricted);
}

#[test]
fn resolve_sandbox_policies_accepts_semantically_equivalent_workspace_write_inputs() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let workspace = temp_dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let workspace = AbsolutePathBuf::from_absolute_path(&workspace).expect("absolute workspace");
    let sandbox_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![workspace],
        read_only_access: ReadOnlyAccess::FullAccess,
        network_access: false,
        exclude_tmpdir_env_var: false,
        exclude_slash_tmp: false,
    };
    let vfs_policy = VfsPolicy::from(&SandboxPolicy::new_workspace_write_policy());

    let resolved = resolve_sandbox_policies(
        temp_dir.path().join("workspace").as_path(),
        Some(sandbox_policy.clone()),
        Some(vfs_policy.clone()),
        Some(SocketPolicy::Restricted),
    )
    .expect("semantically equivalent workspace-write policy should resolve");

    assert_eq!(resolved.sandbox_policy, sandbox_policy);
    assert_eq!(resolved.vfs_policy, vfs_policy);
    assert_eq!(resolved.socket_policy, SocketPolicy::Restricted);
}
