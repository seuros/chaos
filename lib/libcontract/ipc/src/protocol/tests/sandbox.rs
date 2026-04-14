use super::super::*;
use crate::permissions::FileSystemAccessMode;
use crate::permissions::FileSystemPath;
use crate::permissions::FileSystemSandboxEntry;
use crate::permissions::FileSystemSandboxPolicy;
use crate::permissions::FileSystemSpecialPath;
use crate::permissions::NetworkSandboxPolicy;
use chaos_realpath::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use tempfile::TempDir;

fn sorted_writable_roots(roots: Vec<WritableRoot>) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let mut sorted_roots: Vec<(PathBuf, Vec<PathBuf>)> = roots
        .into_iter()
        .map(|root| {
            let mut read_only_subpaths: Vec<PathBuf> = root
                .read_only_subpaths
                .into_iter()
                .map(|path| path.to_path_buf())
                .collect();
            read_only_subpaths.sort();
            (root.root.to_path_buf(), read_only_subpaths)
        })
        .collect();
    sorted_roots.sort_by(|left, right| left.0.cmp(&right.0));
    sorted_roots
}

fn sandbox_policy_allows_read(policy: &SandboxPolicy, path: &Path, cwd: &Path) -> bool {
    if policy.has_full_disk_read_access() {
        return true;
    }

    policy
        .get_readable_roots_with_cwd(cwd)
        .iter()
        .any(|root| path.starts_with(root.as_path()))
        || policy
            .get_writable_roots_with_cwd(cwd)
            .iter()
            .any(|root| path.starts_with(root.root.as_path()))
}

fn sandbox_policy_allows_write(policy: &SandboxPolicy, path: &Path, cwd: &Path) -> bool {
    if policy.has_full_disk_write_access() {
        return true;
    }

    policy
        .get_writable_roots_with_cwd(cwd)
        .iter()
        .any(|root| root.is_path_writable(path))
}

fn sandbox_policy_probe_paths(policy: &SandboxPolicy, cwd: &Path) -> Vec<PathBuf> {
    let mut paths = vec![cwd.to_path_buf()];
    paths.extend(
        policy
            .get_readable_roots_with_cwd(cwd)
            .into_iter()
            .map(|path| path.to_path_buf()),
    );
    for root in policy.get_writable_roots_with_cwd(cwd) {
        paths.push(root.root.to_path_buf());
        paths.extend(
            root.read_only_subpaths
                .into_iter()
                .map(|path| path.to_path_buf()),
        );
    }
    paths.sort();
    paths.dedup();
    paths
}

fn assert_same_sandbox_policy_semantics(
    expected: &SandboxPolicy,
    actual: &SandboxPolicy,
    cwd: &Path,
) {
    assert_eq!(
        actual.has_full_disk_read_access(),
        expected.has_full_disk_read_access()
    );
    assert_eq!(
        actual.has_full_disk_write_access(),
        expected.has_full_disk_write_access()
    );
    assert_eq!(
        actual.has_full_network_access(),
        expected.has_full_network_access()
    );
    assert_eq!(
        actual.include_platform_defaults(),
        expected.include_platform_defaults()
    );
    let mut probe_paths = sandbox_policy_probe_paths(expected, cwd);
    probe_paths.extend(sandbox_policy_probe_paths(actual, cwd));
    probe_paths.sort();
    probe_paths.dedup();

    for path in probe_paths {
        assert_eq!(
            sandbox_policy_allows_read(actual, &path, cwd),
            sandbox_policy_allows_read(expected, &path, cwd),
            "read access mismatch for {}",
            path.display()
        );
        assert_eq!(
            sandbox_policy_allows_write(actual, &path, cwd),
            sandbox_policy_allows_write(expected, &path, cwd),
            "write access mismatch for {}",
            path.display()
        );
    }
}

#[test]
fn external_sandbox_reports_full_access_flags() {
    let restricted = SandboxPolicy::ExternalSandbox {
        network_access: NetworkAccess::Restricted,
    };
    assert!(restricted.has_full_disk_write_access());
    assert!(!restricted.has_full_network_access());

    let enabled = SandboxPolicy::ExternalSandbox {
        network_access: NetworkAccess::Enabled,
    };
    assert!(enabled.has_full_disk_write_access());
    assert!(enabled.has_full_network_access());
}

#[test]
fn read_only_reports_network_access_flags() {
    let restricted = SandboxPolicy::new_read_only_policy();
    assert!(!restricted.has_full_network_access());

    let enabled = SandboxPolicy::ReadOnly {
        access: ReadOnlyAccess::FullAccess,
        network_access: true,
    };
    assert!(enabled.has_full_network_access());
}

#[test]
fn granular_approval_config_mcp_elicitation_flag_is_field_driven() {
    assert!(
        GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            request_permissions: false,
            mcp_elicitations: true,
        }
        .allows_mcp_elicitations()
    );
    assert!(
        !GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            request_permissions: false,
            mcp_elicitations: false,
        }
        .allows_mcp_elicitations()
    );
}

#[test]
fn granular_approval_config_request_permissions_flag_is_field_driven() {
    assert!(
        GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            request_permissions: true,
            mcp_elicitations: false,
        }
        .allows_request_permissions()
    );
    assert!(
        !GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            request_permissions: false,
            mcp_elicitations: false,
        }
        .allows_request_permissions()
    );
}

#[test]
fn granular_approval_config_defaults_missing_optional_flags_to_false() {
    let decoded = serde_json::from_value::<GranularApprovalConfig>(serde_json::json!({
        "sandbox_approval": true,
        "rules": false,
        "mcp_elicitations": true,
    }))
    .expect("granular approval config should deserialize");

    assert_eq!(
        decoded,
        GranularApprovalConfig {
            sandbox_approval: true,
            rules: false,
            request_permissions: false,
            mcp_elicitations: true,
        }
    );
}

#[test]
fn workspace_write_restricted_read_access_includes_effective_writable_roots() {
    let cwd = Path::new("/tmp/workspace");
    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![],
        read_only_access: ReadOnlyAccess::Restricted {
            include_platform_defaults: false,
            readable_roots: vec![],
        },
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: false,
    };

    let readable_roots = policy.get_readable_roots_with_cwd(cwd);
    let writable_roots = policy.get_writable_roots_with_cwd(cwd);

    for writable_root in writable_roots {
        assert!(
            readable_roots
                .iter()
                .any(|root| root.as_path() == writable_root.root.as_path()),
            "expected writable root {} to also be readable",
            writable_root.root.as_path().display()
        );
    }
}

#[test]
fn restricted_file_system_policy_reports_full_access_from_root_entries() {
    let read_only = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        access: FileSystemAccessMode::Read,
    }]);
    assert!(read_only.has_full_disk_read_access());
    assert!(!read_only.has_full_disk_write_access());
    assert!(!read_only.include_platform_defaults());

    let writable = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        access: FileSystemAccessMode::Write,
    }]);
    assert!(writable.has_full_disk_read_access());
    assert!(writable.has_full_disk_write_access());
}

#[test]
fn restricted_file_system_policy_treats_root_with_carveouts_as_scoped_access() {
    let cwd = TempDir::new().expect("tempdir");
    let canonical_cwd = cwd.path().canonicalize().expect("canonicalize cwd");
    let root = AbsolutePathBuf::from_absolute_path(&canonical_cwd)
        .expect("absolute canonical tempdir")
        .as_path()
        .ancestors()
        .last()
        .and_then(|path| AbsolutePathBuf::from_absolute_path(path).ok())
        .expect("filesystem root");
    let blocked =
        AbsolutePathBuf::resolve_path_against_base("blocked", cwd.path()).expect("resolve blocked");
    let expected_blocked = AbsolutePathBuf::from_absolute_path(
        cwd.path()
            .canonicalize()
            .expect("canonicalize cwd")
            .join("blocked"),
    )
    .expect("canonical blocked");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: blocked },
            access: FileSystemAccessMode::None,
        },
    ]);

    assert!(!policy.has_full_disk_read_access());
    assert!(!policy.has_full_disk_write_access());
    assert_eq!(
        policy.get_readable_roots_with_cwd(cwd.path()),
        vec![root.clone()]
    );
    assert_eq!(
        policy.get_unreadable_roots_with_cwd(cwd.path()),
        vec![expected_blocked.clone()]
    );

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(writable_roots[0].root, root);
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .iter()
            .any(|path| path.as_path() == expected_blocked.as_path())
    );
}

#[test]
fn restricted_file_system_policy_derives_effective_paths() {
    let cwd = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(cwd.path().join(".agents")).expect("create .agents");
    std::fs::create_dir_all(cwd.path().join(".chaos")).expect("create .chaos");
    let canonical_cwd = cwd.path().canonicalize().expect("canonicalize cwd");
    let cwd_absolute =
        AbsolutePathBuf::from_absolute_path(&canonical_cwd).expect("absolute tempdir");
    let secret = AbsolutePathBuf::resolve_path_against_base("secret", cwd.path())
        .expect("resolve unreadable path");
    let expected_secret = AbsolutePathBuf::from_absolute_path(canonical_cwd.join("secret"))
        .expect("canonical secret");
    let expected_agents = AbsolutePathBuf::from_absolute_path(canonical_cwd.join(".agents"))
        .expect("canonical .agents");
    let expected_chaos = AbsolutePathBuf::from_absolute_path(canonical_cwd.join(".chaos"))
        .expect("canonical .chaos");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Minimal,
            },
            access: FileSystemAccessMode::Read,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::CurrentWorkingDirectory,
            },
            access: FileSystemAccessMode::Write,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: secret },
            access: FileSystemAccessMode::None,
        },
    ]);

    assert!(!policy.has_full_disk_read_access());
    assert!(!policy.has_full_disk_write_access());
    assert!(policy.include_platform_defaults());
    assert_eq!(
        policy.get_readable_roots_with_cwd(cwd.path()),
        vec![cwd_absolute.clone()]
    );
    assert_eq!(
        policy.get_unreadable_roots_with_cwd(cwd.path()),
        vec![expected_secret.clone()]
    );

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(writable_roots[0].root, cwd_absolute);
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .iter()
            .any(|path| path.as_path() == expected_secret.as_path())
    );
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .iter()
            .any(|path| path.as_path() == expected_agents.as_path())
    );
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .iter()
            .any(|path| path.as_path() == expected_chaos.as_path())
    );
}

#[test]
fn restricted_file_system_policy_treats_read_entries_as_read_only_subpaths() {
    let cwd = TempDir::new().expect("tempdir");
    let canonical_cwd = cwd.path().canonicalize().expect("canonicalize cwd");
    let docs =
        AbsolutePathBuf::resolve_path_against_base("docs", cwd.path()).expect("resolve docs");
    let docs_public = AbsolutePathBuf::resolve_path_against_base("docs/public", cwd.path())
        .expect("resolve docs/public");
    let expected_docs =
        AbsolutePathBuf::from_absolute_path(canonical_cwd.join("docs")).expect("canonical docs");
    let expected_docs_public =
        AbsolutePathBuf::from_absolute_path(canonical_cwd.join("docs/public"))
            .expect("canonical docs/public");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::CurrentWorkingDirectory,
            },
            access: FileSystemAccessMode::Write,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: docs },
            access: FileSystemAccessMode::Read,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: docs_public },
            access: FileSystemAccessMode::Write,
        },
    ]);

    assert!(!policy.has_full_disk_write_access());
    assert_eq!(
        sorted_writable_roots(policy.get_writable_roots_with_cwd(cwd.path())),
        vec![
            (canonical_cwd, vec![expected_docs.to_path_buf()]),
            (expected_docs_public.to_path_buf(), Vec::new()),
        ]
    );
}

#[test]
fn legacy_workspace_write_nested_readable_root_stays_writable() {
    let cwd = TempDir::new().expect("tempdir");
    let docs =
        AbsolutePathBuf::resolve_path_against_base("docs", cwd.path()).expect("resolve docs");
    let canonical_cwd = cwd.path().canonicalize().expect("canonicalize cwd");
    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![],
        read_only_access: ReadOnlyAccess::Restricted {
            include_platform_defaults: true,
            readable_roots: vec![docs],
        },
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };

    assert_eq!(
        sorted_writable_roots(
            FileSystemSandboxPolicy::from_legacy_sandbox_policy(&policy, cwd.path())
                .get_writable_roots_with_cwd(cwd.path())
        ),
        vec![(canonical_cwd, Vec::new())]
    );
}

#[test]
fn file_system_policy_rejects_legacy_bridge_for_non_workspace_writes() {
    let cwd = Path::new("/tmp/workspace");
    let external_write_path =
        AbsolutePathBuf::from_absolute_path("/tmp").expect("absolute tmp path");
    let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: external_write_path,
        },
        access: FileSystemAccessMode::Write,
    }]);

    let err = policy
        .to_legacy_sandbox_policy(NetworkSandboxPolicy::Restricted, cwd)
        .expect_err("non-workspace writes should be rejected");

    assert!(
        err.to_string()
            .contains("filesystem writes outside the workspace root"),
        "{err}"
    );
}

#[test]
fn legacy_sandbox_policy_semantics_survive_split_bridge() {
    let cwd = TempDir::new().expect("tempdir");
    let readable_root = AbsolutePathBuf::resolve_path_against_base("readable", cwd.path())
        .expect("resolve readable root");
    let writable_root = AbsolutePathBuf::resolve_path_against_base("writable", cwd.path())
        .expect("resolve writable root");
    let nested_readable_root = AbsolutePathBuf::resolve_path_against_base("docs", cwd.path())
        .expect("resolve nested readable root");
    let policies = [
        SandboxPolicy::RootAccess,
        SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Restricted,
        },
        SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Enabled,
        },
        SandboxPolicy::ReadOnly {
            access: ReadOnlyAccess::FullAccess,
            network_access: false,
        },
        SandboxPolicy::ReadOnly {
            access: ReadOnlyAccess::Restricted {
                include_platform_defaults: true,
                readable_roots: vec![readable_root.clone()],
            },
            network_access: true,
        },
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            read_only_access: ReadOnlyAccess::FullAccess,
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![writable_root],
            read_only_access: ReadOnlyAccess::Restricted {
                include_platform_defaults: true,
                readable_roots: vec![readable_root],
            },
            network_access: true,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: true,
        },
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            read_only_access: ReadOnlyAccess::Restricted {
                include_platform_defaults: true,
                readable_roots: vec![nested_readable_root],
            },
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
    ];

    for expected in policies {
        let actual = FileSystemSandboxPolicy::from_legacy_sandbox_policy(&expected, cwd.path())
            .to_legacy_sandbox_policy(NetworkSandboxPolicy::from(&expected), cwd.path())
            .expect("legacy bridge should preserve legacy policy semantics");

        assert_same_sandbox_policy_semantics(&expected, &actual, cwd.path());
    }
}
