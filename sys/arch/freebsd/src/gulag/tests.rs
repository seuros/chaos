use super::*;
use chaos_ipc::permissions::{VfsAccessMode, VfsEntry, VfsPath};
use chaos_ipc::protocol::SandboxPolicy;
use chaos_realpath::AbsolutePathBuf;

#[test]
fn readonly_requires_warden_not_a_network_only_label() {
    let result = build_label(
        &VfsPolicy::default(),
        SocketPolicy::Restricted,
        Path::new("/tmp"),
    );
    assert!(result.is_err());
}

#[test]
fn network_only_preserves_full_write_access() {
    let label = build_label(
        &VfsPolicy::unrestricted(),
        SocketPolicy::Restricted,
        Path::new("/tmp"),
    )
    .unwrap();
    assert_eq!(label, "gulag/restrict!v=1!net=deny");
}

#[test]
fn restricted_reads_fail_closed() {
    let result = build_label(
        &VfsPolicy::restricted(Vec::new()),
        SocketPolicy::Enabled,
        Path::new("/tmp"),
    );
    assert!(result.is_err());
}

#[test]
fn explicit_read_denials_fail_closed() {
    let mut vfs = VfsPolicy::default();
    vfs.entries.push(VfsEntry {
        path: VfsPath::Path {
            path: AbsolutePathBuf::from_absolute_path("/etc").unwrap(),
        },
        access: VfsAccessMode::None,
    });
    assert!(build_label(&vfs, SocketPolicy::Enabled, Path::new("/tmp")).is_err());
}

#[test]
fn workspace_cannot_be_mistaken_for_driver_enforcement() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let vfs =
        VfsPolicy::from_sandbox_policy(&SandboxPolicy::new_workspace_write_policy(), root.path());
    assert!(build_label(&vfs, SocketPolicy::Enabled, root.path()).is_err());
}

#[test]
fn network_enabled_label_has_no_filesystem_claims() {
    let label = build_label(
        &VfsPolicy::unrestricted(),
        SocketPolicy::Enabled,
        Path::new("/tmp"),
    )
    .unwrap();
    assert_eq!(label, "gulag/restrict!v=1");
}

#[test]
fn full_read_and_write_are_both_required_for_direct_execution() {
    let mut vfs = VfsPolicy::default();
    for entry in &mut vfs.entries {
        entry.access = VfsAccessMode::Write;
    }
    vfs.entries.push(VfsEntry {
        path: VfsPath::Path {
            path: AbsolutePathBuf::from_absolute_path("/etc").unwrap(),
        },
        access: VfsAccessMode::None,
    });
    assert!(build_label(&vfs, SocketPolicy::Enabled, Path::new("/tmp")).is_err());
}
