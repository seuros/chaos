use super::*;
use chaos_ipc::permissions::{VfsAccessMode, VfsEntry, VfsPath};
use chaos_ipc::protocol::SandboxPolicy;
use chaos_realpath::AbsolutePathBuf;
use std::os::unix::fs::symlink;

fn entry(path: &Path, access: VfsAccessMode) -> VfsEntry {
    VfsEntry {
        path: VfsPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).expect("test absolute path"),
        },
        access,
    }
}

fn overlays(plan: &WardenPlan) -> &[NullfsOverlay] {
    match &plan.filesystem {
        FilesystemPlan::ReadOnlyHostView { overlays } => overlays,
        FilesystemPlan::Unrestricted => panic!("test expected a restricted plan"),
    }
}

#[test]
fn unrestricted_filesystem_keeps_process_policy_separate() {
    let plan = compile(
        &VfsPolicy::unrestricted(),
        SocketPolicy::Restricted,
        Path::new("/tmp"),
    )
    .unwrap();
    assert!(!plan.requires_warden());
    assert_eq!(plan.credential_label, "gulag/restrict!v=1!net=deny");
}

#[test]
fn readonly_plan_requires_warden_even_without_overlays() {
    let plan = compile(
        &VfsPolicy::default(),
        SocketPolicy::Enabled,
        Path::new("/tmp"),
    )
    .unwrap();
    assert!(plan.requires_warden());
    assert!(overlays(&plan).is_empty());
    assert_eq!(plan.credential_label, "gulag/restrict!v=1");
}

#[test]
fn workspace_orders_writable_parent_before_readonly_carveouts() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join(".git")).unwrap();
    std::fs::create_dir(temp.path().join(".chaos")).unwrap();
    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![],
        read_only_access: Default::default(),
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    let vfs = VfsPolicy::from_sandbox_policy(&policy, temp.path());
    let plan = compile(&vfs, SocketPolicy::Restricted, temp.path()).unwrap();
    let mounts = overlays(&plan);
    let root = std::fs::canonicalize(temp.path()).unwrap();
    assert_eq!(mounts[0].source, root);
    assert_eq!(mounts[0].access, OverlayAccess::ReadWrite);
    for name in [".git", ".chaos"] {
        let index = mounts
            .iter()
            .position(|m| m.jail_path == root.join(name))
            .unwrap();
        assert!(index > 0);
        assert_eq!(mounts[index].access, OverlayAccess::ReadOnly);
    }
}

#[test]
fn missing_carveout_is_not_silently_dropped_or_created() {
    let temp = tempfile::tempdir().unwrap();
    let allowed = temp.path().join("allowed");
    std::fs::create_dir(&allowed).unwrap();
    let missing = allowed.join("protected");
    let mut vfs = VfsPolicy::default();
    vfs.entries.push(entry(&allowed, VfsAccessMode::Write));
    vfs.entries.push(entry(&missing, VfsAccessMode::Read));
    assert!(compile(&vfs, SocketPolicy::Enabled, temp.path()).is_err());
    assert!(!missing.exists());
}

#[test]
fn alias_sources_are_canonical_and_paths_are_not_shell_text() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("a!w=, %\n");
    let alias = temp.path().join("alias");
    std::fs::create_dir(&real).unwrap();
    symlink(&real, &alias).unwrap();
    let mut vfs = VfsPolicy::default();
    vfs.entries.push(entry(&alias, VfsAccessMode::Write));
    let plan = compile(&vfs, SocketPolicy::Enabled, temp.path()).unwrap();
    let canonical = std::fs::canonicalize(&real).unwrap();
    assert_eq!(overlays(&plan)[0].source, canonical);
    assert_eq!(overlays(&plan)[0].jail_path, canonical);
    assert_eq!(plan.credential_label, "gulag/restrict!v=1");
}

#[test]
fn dangling_alias_and_missing_writable_root_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let alias = temp.path().join("dangling");
    symlink(temp.path().join("missing"), &alias).unwrap();
    for path in [&alias, &temp.path().join("new")] {
        let mut vfs = VfsPolicy::default();
        vfs.entries.push(entry(path, VfsAccessMode::Write));
        assert!(compile(&vfs, SocketPolicy::Enabled, temp.path()).is_err());
    }
}

#[test]
fn restricted_reads_and_read_denials_are_not_broadened() {
    assert!(
        compile(
            &VfsPolicy::restricted(Vec::new()),
            SocketPolicy::Enabled,
            Path::new("/tmp"),
        )
        .is_err()
    );
    let mut vfs = VfsPolicy::default();
    for entry in &mut vfs.entries {
        entry.access = VfsAccessMode::Write;
    }
    vfs.entries
        .push(entry(Path::new("/etc"), VfsAccessMode::None));
    assert!(compile(&vfs, SocketPolicy::Enabled, Path::new("/tmp")).is_err());
}

#[test]
fn paths_reject_parent_components_and_nul_before_source_resolution() {
    assert!(validate_path(Path::new("/tmp/../etc")).is_err());
    assert!(validate_path(Path::new("relative")).is_err());
    assert!(validate_path(Path::new("/tmp/nul\0path")).is_err());
}

#[test]
fn readonly_symlink_paths_are_rejected_not_replaced_by_the_target() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    let alias = temp.path().join("alias");
    std::fs::create_dir(&target).unwrap();
    std::fs::create_dir(target.join("child")).unwrap();
    symlink(&target, &alias).unwrap();
    assert!(existing_carveout(&alias).is_err());
    assert!(existing_carveout(&alias.join("child")).is_err());
}
