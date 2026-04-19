use super::*;
use chaos_realpath::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

use crate::protocol::ReadOnlyAccess;
use crate::protocol::SandboxPolicy;

const SYMLINKED_TMPDIR_TEST_ENV: &str = "CHAOS_PROTOCOL_TEST_SYMLINKED_TMPDIR";

fn symlink_dir(original: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

#[test]
fn unknown_special_paths_are_ignored_by_legacy_bridge() -> std::io::Result<()> {
    let policy = VfsPolicy::restricted(vec![VfsEntry {
        path: VfsPath::Special {
            value: VfsSpecialPath::unknown(":future_special_path", None),
        },
        access: VfsAccessMode::Write,
    }]);

    let sandbox_policy =
        policy.to_sandbox_policy(SocketPolicy::Restricted, Path::new("/tmp/workspace"))?;

    assert_eq!(
        sandbox_policy,
        SandboxPolicy::ReadOnly {
            access: ReadOnlyAccess::Restricted {
                include_platform_defaults: false,
                readable_roots: Vec::new(),
            },
            network_access: false,
        }
    );
    Ok(())
}

#[test]
fn effective_runtime_roots_canonicalize_symlinked_paths() {
    let cwd = TempDir::new().expect("tempdir");
    let real_root = cwd.path().join("real");
    let link_root = cwd.path().join("link");
    let blocked = real_root.join("blocked");
    let chaos_dir = real_root.join(".chaos");

    fs::create_dir_all(&blocked).expect("create blocked");
    fs::create_dir_all(&chaos_dir).expect("create .chaos");
    symlink_dir(&real_root, &link_root).expect("create symlinked root");

    let link_root =
        AbsolutePathBuf::from_absolute_path(&link_root).expect("absolute symlinked root");
    let link_blocked = link_root.join("blocked").expect("symlinked blocked path");
    let expected_root = AbsolutePathBuf::from_absolute_path(
        real_root.canonicalize().expect("canonicalize real root"),
    )
    .expect("absolute canonical root");
    let expected_blocked =
        AbsolutePathBuf::from_absolute_path(blocked.canonicalize().expect("canonicalize blocked"))
            .expect("absolute canonical blocked");
    let expected_codex =
        AbsolutePathBuf::from_absolute_path(chaos_dir.canonicalize().expect("canonicalize .chaos"))
            .expect("absolute canonical .chaos");

    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Path { path: link_root },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: link_blocked },
            access: VfsAccessMode::None,
        },
    ]);

    assert_eq!(
        policy.get_unreadable_roots_with_cwd(cwd.path()),
        vec![expected_blocked.clone()]
    );

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(writable_roots[0].root, expected_root);
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .contains(&expected_blocked)
    );
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .contains(&expected_codex)
    );
}

#[test]
fn writable_roots_preserve_symlinked_protected_subpaths() {
    let cwd = TempDir::new().expect("tempdir");
    let root = cwd.path().join("root");
    let decoy = root.join("decoy-chaos");
    let dot_chaos = root.join(".chaos");
    fs::create_dir_all(&decoy).expect("create decoy");
    symlink_dir(&decoy, &dot_chaos).expect("create .chaos symlink");

    let root = AbsolutePathBuf::from_absolute_path(&root).expect("absolute root");
    let expected_dot_codex = AbsolutePathBuf::from_absolute_path(
        root.as_path()
            .canonicalize()
            .expect("canonicalize root")
            .join(".chaos"),
    )
    .expect("absolute .chaos symlink");
    let unexpected_decoy =
        AbsolutePathBuf::from_absolute_path(decoy.canonicalize().expect("canonicalize decoy"))
            .expect("absolute canonical decoy");

    let policy = VfsPolicy::restricted(vec![VfsEntry {
        path: VfsPath::Path { path: root },
        access: VfsAccessMode::Write,
    }]);

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(
        writable_roots[0].read_only_subpaths,
        vec![expected_dot_codex]
    );
    assert!(
        !writable_roots[0]
            .read_only_subpaths
            .contains(&unexpected_decoy)
    );
}

#[test]
fn writable_roots_preserve_explicit_symlinked_carveouts_under_symlinked_roots() {
    let cwd = TempDir::new().expect("tempdir");
    let real_root = cwd.path().join("real");
    let link_root = cwd.path().join("link");
    let decoy = real_root.join("decoy-private");
    let linked_private = real_root.join("linked-private");
    fs::create_dir_all(&decoy).expect("create decoy");
    symlink_dir(&real_root, &link_root).expect("create symlinked root");
    symlink_dir(&decoy, &linked_private).expect("create linked-private symlink");

    let link_root =
        AbsolutePathBuf::from_absolute_path(&link_root).expect("absolute symlinked root");
    let link_private = link_root
        .join("linked-private")
        .expect("symlinked linked-private path");
    let expected_root = AbsolutePathBuf::from_absolute_path(
        real_root.canonicalize().expect("canonicalize real root"),
    )
    .expect("absolute canonical root");
    let expected_linked_private = expected_root
        .join("linked-private")
        .expect("expected linked-private path");
    let unexpected_decoy =
        AbsolutePathBuf::from_absolute_path(decoy.canonicalize().expect("canonicalize decoy"))
            .expect("absolute canonical decoy");

    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Path { path: link_root },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: link_private },
            access: VfsAccessMode::None,
        },
    ]);

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(writable_roots[0].root, expected_root);
    assert_eq!(
        writable_roots[0].read_only_subpaths,
        vec![expected_linked_private]
    );
    assert!(
        !writable_roots[0]
            .read_only_subpaths
            .contains(&unexpected_decoy)
    );
}

#[test]
fn writable_roots_preserve_explicit_symlinked_carveouts_that_escape_root() {
    let cwd = TempDir::new().expect("tempdir");
    let real_root = cwd.path().join("real");
    let link_root = cwd.path().join("link");
    let decoy = cwd.path().join("outside-private");
    let linked_private = real_root.join("linked-private");
    fs::create_dir_all(&decoy).expect("create decoy");
    fs::create_dir_all(&real_root).expect("create real root");
    symlink_dir(&real_root, &link_root).expect("create symlinked root");
    symlink_dir(&decoy, &linked_private).expect("create linked-private symlink");

    let link_root =
        AbsolutePathBuf::from_absolute_path(&link_root).expect("absolute symlinked root");
    let link_private = link_root
        .join("linked-private")
        .expect("symlinked linked-private path");
    let expected_root = AbsolutePathBuf::from_absolute_path(
        real_root.canonicalize().expect("canonicalize real root"),
    )
    .expect("absolute canonical root");
    let expected_linked_private = expected_root
        .join("linked-private")
        .expect("expected linked-private path");
    let unexpected_decoy =
        AbsolutePathBuf::from_absolute_path(decoy.canonicalize().expect("canonicalize decoy"))
            .expect("absolute canonical decoy");

    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Path { path: link_root },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: link_private },
            access: VfsAccessMode::None,
        },
    ]);

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(writable_roots[0].root, expected_root);
    assert_eq!(
        writable_roots[0].read_only_subpaths,
        vec![expected_linked_private]
    );
    assert!(
        !writable_roots[0]
            .read_only_subpaths
            .contains(&unexpected_decoy)
    );
}

#[test]
fn writable_roots_preserve_explicit_symlinked_carveouts_that_alias_root() {
    let cwd = TempDir::new().expect("tempdir");
    let root = cwd.path().join("root");
    let alias = root.join("alias-root");
    fs::create_dir_all(&root).expect("create root");
    symlink_dir(&root, &alias).expect("create alias symlink");

    let root = AbsolutePathBuf::from_absolute_path(&root).expect("absolute root");
    let alias = root.join("alias-root").expect("alias root path");
    let expected_root = AbsolutePathBuf::from_absolute_path(
        root.as_path().canonicalize().expect("canonicalize root"),
    )
    .expect("absolute canonical root");
    let expected_alias = expected_root
        .join("alias-root")
        .expect("expected alias path");

    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Path { path: root },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: alias },
            access: VfsAccessMode::None,
        },
    ]);

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(writable_roots[0].root, expected_root);
    assert_eq!(writable_roots[0].read_only_subpaths, vec![expected_alias]);
}

#[test]
fn tmpdir_special_path_canonicalizes_symlinked_tmpdir() {
    if std::env::var_os(SYMLINKED_TMPDIR_TEST_ENV).is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .env(SYMLINKED_TMPDIR_TEST_ENV, "1")
            .arg("--exact")
            .arg("permissions::tests::tmpdir_special_path_canonicalizes_symlinked_tmpdir")
            .output()
            .expect("run tmpdir subprocess test");

        assert!(
            output.status.success(),
            "tmpdir subprocess test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let real_tmpdir = cwd.path().join("real-tmpdir");
    let link_tmpdir = cwd.path().join("link-tmpdir");
    let blocked = real_tmpdir.join("blocked");
    let chaos_dir = real_tmpdir.join(".chaos");

    fs::create_dir_all(&blocked).expect("create blocked");
    fs::create_dir_all(&chaos_dir).expect("create .chaos");
    symlink_dir(&real_tmpdir, &link_tmpdir).expect("create symlinked tmpdir");

    let link_blocked =
        AbsolutePathBuf::from_absolute_path(link_tmpdir.join("blocked")).expect("link blocked");
    let expected_root = AbsolutePathBuf::from_absolute_path(
        real_tmpdir
            .canonicalize()
            .expect("canonicalize real tmpdir"),
    )
    .expect("absolute canonical tmpdir");
    let expected_blocked =
        AbsolutePathBuf::from_absolute_path(blocked.canonicalize().expect("canonicalize blocked"))
            .expect("absolute canonical blocked");
    let expected_codex =
        AbsolutePathBuf::from_absolute_path(chaos_dir.canonicalize().expect("canonicalize .chaos"))
            .expect("absolute canonical .chaos");

    unsafe {
        std::env::set_var("TMPDIR", &link_tmpdir);
    }

    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::Tmpdir,
            },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: link_blocked },
            access: VfsAccessMode::None,
        },
    ]);

    assert_eq!(
        policy.get_unreadable_roots_with_cwd(cwd.path()),
        vec![expected_blocked.clone()]
    );

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(writable_roots[0].root, expected_root);
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .contains(&expected_blocked)
    );
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .contains(&expected_codex)
    );
}

#[test]
fn resolve_access_with_cwd_uses_most_specific_entry() {
    let cwd = TempDir::new().expect("tempdir");
    let docs =
        AbsolutePathBuf::resolve_path_against_base("docs", cwd.path()).expect("resolve docs");
    let docs_private = AbsolutePathBuf::resolve_path_against_base("docs/private", cwd.path())
        .expect("resolve docs/private");
    let docs_private_public =
        AbsolutePathBuf::resolve_path_against_base("docs/private/public", cwd.path())
            .expect("resolve docs/private/public");
    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::CurrentWorkingDirectory,
            },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: docs.clone() },
            access: VfsAccessMode::Read,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: docs_private.clone(),
            },
            access: VfsAccessMode::None,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: docs_private_public.clone(),
            },
            access: VfsAccessMode::Write,
        },
    ]);

    assert_eq!(
        policy.resolve_access_with_cwd(cwd.path(), cwd.path()),
        VfsAccessMode::Write
    );
    assert_eq!(
        policy.resolve_access_with_cwd(docs.as_path(), cwd.path()),
        VfsAccessMode::Read
    );
    assert_eq!(
        policy.resolve_access_with_cwd(docs_private.as_path(), cwd.path()),
        VfsAccessMode::None
    );
    assert_eq!(
        policy.resolve_access_with_cwd(docs_private_public.as_path(), cwd.path()),
        VfsAccessMode::Write
    );
}

#[test]
fn split_only_nested_carveouts_need_direct_runtime_enforcement() {
    let cwd = TempDir::new().expect("tempdir");
    let docs =
        AbsolutePathBuf::resolve_path_against_base("docs", cwd.path()).expect("resolve docs");
    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::CurrentWorkingDirectory,
            },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: docs },
            access: VfsAccessMode::Read,
        },
    ]);

    assert!(policy.needs_direct_runtime_enforcement(SocketPolicy::Restricted, cwd.path(),));

    let legacy_workspace_write = VfsPolicy::from(&SandboxPolicy::new_workspace_write_policy());
    assert!(
        !legacy_workspace_write
            .needs_direct_runtime_enforcement(SocketPolicy::Restricted, cwd.path(),)
    );
}

#[test]
fn root_write_with_read_only_child_is_not_full_disk_write() {
    let cwd = TempDir::new().expect("tempdir");
    let docs =
        AbsolutePathBuf::resolve_path_against_base("docs", cwd.path()).expect("resolve docs");
    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::Root,
            },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: docs.clone() },
            access: VfsAccessMode::Read,
        },
    ]);

    assert!(!policy.has_full_disk_write_access());
    assert_eq!(
        policy.resolve_access_with_cwd(docs.as_path(), cwd.path()),
        VfsAccessMode::Read
    );
    assert!(policy.needs_direct_runtime_enforcement(SocketPolicy::Restricted, cwd.path(),));
}

#[test]
fn root_deny_does_not_materialize_as_unreadable_root() {
    let cwd = TempDir::new().expect("tempdir");
    let docs =
        AbsolutePathBuf::resolve_path_against_base("docs", cwd.path()).expect("resolve docs");
    let expected_docs = AbsolutePathBuf::from_absolute_path(
        cwd.path()
            .canonicalize()
            .expect("canonicalize cwd")
            .join("docs"),
    )
    .expect("canonical docs");
    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::Root,
            },
            access: VfsAccessMode::None,
        },
        VfsEntry {
            path: VfsPath::Path { path: docs.clone() },
            access: VfsAccessMode::Read,
        },
    ]);

    assert_eq!(
        policy.resolve_access_with_cwd(docs.as_path(), cwd.path()),
        VfsAccessMode::Read
    );
    assert_eq!(
        policy.get_readable_roots_with_cwd(cwd.path()),
        vec![expected_docs]
    );
    assert!(policy.get_unreadable_roots_with_cwd(cwd.path()).is_empty());
}

#[test]
fn duplicate_root_deny_prevents_full_disk_write_access() {
    let cwd = TempDir::new().expect("tempdir");
    let root = AbsolutePathBuf::from_absolute_path(cwd.path())
        .map(|cwd| absolute_vfs_root_path_for_cwd(&cwd))
        .expect("resolve filesystem root");
    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::Root,
            },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::Root,
            },
            access: VfsAccessMode::None,
        },
    ]);

    assert!(!policy.has_full_disk_write_access());
    assert_eq!(
        policy.resolve_access_with_cwd(root.as_path(), cwd.path()),
        VfsAccessMode::None
    );
}

#[test]
fn same_specificity_write_override_keeps_full_disk_write_access() {
    let cwd = TempDir::new().expect("tempdir");
    let docs =
        AbsolutePathBuf::resolve_path_against_base("docs", cwd.path()).expect("resolve docs");
    let policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::Root,
            },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path { path: docs.clone() },
            access: VfsAccessMode::Read,
        },
        VfsEntry {
            path: VfsPath::Path { path: docs.clone() },
            access: VfsAccessMode::Write,
        },
    ]);

    assert!(policy.has_full_disk_write_access());
    assert_eq!(
        policy.resolve_access_with_cwd(docs.as_path(), cwd.path()),
        VfsAccessMode::Write
    );
}

#[test]
fn file_system_access_mode_orders_by_conflict_precedence() {
    assert!(VfsAccessMode::Write > VfsAccessMode::Read);
    assert!(VfsAccessMode::None > VfsAccessMode::Write);
}
