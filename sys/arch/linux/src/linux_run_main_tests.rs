#[cfg(test)]
use super::*;
#[cfg(test)]
use chaos_ipc::protocol::FileSystemSandboxPolicy;
#[cfg(test)]
use chaos_ipc::protocol::NetworkSandboxPolicy;
#[cfg(test)]
use chaos_ipc::protocol::ReadOnlyAccess;
#[cfg(test)]
use chaos_ipc::protocol::SandboxPolicy;
#[cfg(test)]
use chaos_realpath::AbsolutePathBuf;
#[cfg(test)]
use pretty_assertions::assert_eq;

#[test]
fn split_only_filesystem_policy_requires_direct_runtime_enforcement() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let docs = temp_dir.path().join("docs");
    std::fs::create_dir_all(&docs).expect("create docs");
    let docs = AbsolutePathBuf::from_absolute_path(&docs).expect("absolute docs");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        chaos_ipc::permissions::FileSystemSandboxEntry {
            path: chaos_ipc::permissions::FileSystemPath::Special {
                value: chaos_ipc::permissions::FileSystemSpecialPath::CurrentWorkingDirectory,
            },
            access: chaos_ipc::permissions::FileSystemAccessMode::Write,
        },
        chaos_ipc::permissions::FileSystemSandboxEntry {
            path: chaos_ipc::permissions::FileSystemPath::Path { path: docs },
            access: chaos_ipc::permissions::FileSystemAccessMode::Read,
        },
    ]);

    assert!(
        policy.needs_direct_runtime_enforcement(NetworkSandboxPolicy::Restricted, temp_dir.path(),)
    );
}

#[test]
fn root_write_read_only_carveout_requires_direct_runtime_enforcement() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let docs = temp_dir.path().join("docs");
    std::fs::create_dir_all(&docs).expect("create docs");
    let docs = AbsolutePathBuf::from_absolute_path(&docs).expect("absolute docs");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        chaos_ipc::permissions::FileSystemSandboxEntry {
            path: chaos_ipc::permissions::FileSystemPath::Special {
                value: chaos_ipc::permissions::FileSystemSpecialPath::Root,
            },
            access: chaos_ipc::permissions::FileSystemAccessMode::Write,
        },
        chaos_ipc::permissions::FileSystemSandboxEntry {
            path: chaos_ipc::permissions::FileSystemPath::Path { path: docs },
            access: chaos_ipc::permissions::FileSystemAccessMode::Read,
        },
    ]);

    assert!(
        policy.needs_direct_runtime_enforcement(NetworkSandboxPolicy::Restricted, temp_dir.path(),)
    );
}

#[test]
fn resolve_sandbox_policies_derives_split_policies_from_legacy_policy() {
    let sandbox_policy = SandboxPolicy::new_read_only_policy();

    let resolved =
        resolve_sandbox_policies(Path::new("/tmp"), Some(sandbox_policy.clone()), None, None)
            .expect("legacy policy should resolve");

    assert_eq!(resolved.sandbox_policy, sandbox_policy);
    assert_eq!(
        resolved.file_system_sandbox_policy,
        FileSystemSandboxPolicy::from(&sandbox_policy)
    );
    assert_eq!(
        resolved.network_sandbox_policy,
        NetworkSandboxPolicy::from(&sandbox_policy)
    );
}

#[test]
fn resolve_sandbox_policies_derives_legacy_policy_from_split_policies() {
    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let file_system_sandbox_policy = FileSystemSandboxPolicy::from(&sandbox_policy);
    let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);

    let resolved = resolve_sandbox_policies(
        Path::new("/tmp"),
        None,
        Some(file_system_sandbox_policy.clone()),
        Some(network_sandbox_policy),
    )
    .expect("split policies should resolve");

    assert_eq!(resolved.sandbox_policy, sandbox_policy);
    assert_eq!(
        resolved.file_system_sandbox_policy,
        file_system_sandbox_policy
    );
    assert_eq!(resolved.network_sandbox_policy, network_sandbox_policy);
}

#[test]
fn resolve_sandbox_policies_rejects_partial_split_policies() {
    let err = resolve_sandbox_policies(
        Path::new("/tmp"),
        Some(SandboxPolicy::new_read_only_policy()),
        Some(FileSystemSandboxPolicy::default()),
        None,
    )
    .expect_err("partial split policies should fail");

    assert_eq!(err, ResolveSandboxPoliciesError::PartialSplitPolicies);
}

#[test]
fn resolve_sandbox_policies_rejects_mismatched_legacy_and_split_inputs() {
    let err = resolve_sandbox_policies(
        Path::new("/tmp"),
        Some(SandboxPolicy::new_read_only_policy()),
        Some(FileSystemSandboxPolicy::unrestricted()),
        Some(NetworkSandboxPolicy::Enabled),
    )
    .expect_err("mismatched legacy and split policies should fail");
    assert!(
        matches!(
            err,
            ResolveSandboxPoliciesError::MismatchedLegacyPolicy { .. }
        ),
        "{err}"
    );
}

#[test]
fn resolve_sandbox_policies_accepts_split_policies_requiring_direct_runtime_enforcement() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let docs = temp_dir.path().join("docs");
    std::fs::create_dir_all(&docs).expect("create docs");
    let docs = AbsolutePathBuf::from_absolute_path(&docs).expect("absolute docs");
    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        chaos_ipc::permissions::FileSystemSandboxEntry {
            path: chaos_ipc::permissions::FileSystemPath::Special {
                value: chaos_ipc::permissions::FileSystemSpecialPath::Root,
            },
            access: chaos_ipc::permissions::FileSystemAccessMode::Read,
        },
        chaos_ipc::permissions::FileSystemSandboxEntry {
            path: chaos_ipc::permissions::FileSystemPath::Path { path: docs },
            access: chaos_ipc::permissions::FileSystemAccessMode::Write,
        },
    ]);

    let resolved = resolve_sandbox_policies(
        temp_dir.path(),
        Some(sandbox_policy.clone()),
        Some(file_system_sandbox_policy.clone()),
        Some(NetworkSandboxPolicy::Restricted),
    )
    .expect("split-only policy should preserve provided legacy fallback");

    assert_eq!(resolved.sandbox_policy, sandbox_policy);
    assert_eq!(
        resolved.file_system_sandbox_policy,
        file_system_sandbox_policy
    );
    assert_eq!(
        resolved.network_sandbox_policy,
        NetworkSandboxPolicy::Restricted
    );
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
    let file_system_sandbox_policy =
        FileSystemSandboxPolicy::from(&SandboxPolicy::new_workspace_write_policy());

    let resolved = resolve_sandbox_policies(
        temp_dir.path().join("workspace").as_path(),
        Some(sandbox_policy.clone()),
        Some(file_system_sandbox_policy.clone()),
        Some(NetworkSandboxPolicy::Restricted),
    )
    .expect("semantically equivalent legacy workspace-write policy should resolve");

    assert_eq!(resolved.sandbox_policy, sandbox_policy);
    assert_eq!(
        resolved.file_system_sandbox_policy,
        file_system_sandbox_policy
    );
    assert_eq!(
        resolved.network_sandbox_policy,
        NetworkSandboxPolicy::Restricted
    );
}
