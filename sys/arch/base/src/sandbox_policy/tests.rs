use super::*;

#[test]
fn equivalent_policy_representations_resolve_to_the_same_permissions() {
    let sandbox = SandboxPolicy::new_read_only_policy();
    let vfs = VfsPolicy::from(&sandbox);
    let socket = SocketPolicy::from(&sandbox);
    for (sandbox_input, vfs_input, socket_input) in [
        (Some(sandbox.clone()), None, None),
        (None, Some(vfs.clone()), Some(socket)),
        (Some(sandbox.clone()), Some(vfs.clone()), Some(socket)),
    ] {
        let resolved =
            resolve_sandbox_policies(Path::new("/tmp"), sandbox_input, vfs_input, socket_input)
                .expect("equivalent policies should resolve");
        assert_eq!(resolved.sandbox_policy, sandbox);
        assert_eq!(resolved.vfs_policy, vfs);
        assert_eq!(resolved.socket_policy, socket);
    }
}

#[test]
fn incomplete_and_conflicting_policies_are_rejected() {
    for (vfs, socket) in [
        (Some(VfsPolicy::default()), None),
        (None, Some(SocketPolicy::Restricted)),
    ] {
        assert_eq!(
            resolve_sandbox_policies(
                Path::new("/tmp"),
                Some(SandboxPolicy::new_read_only_policy()),
                vfs,
                socket,
            )
            .expect_err("partial split policy"),
            ResolveSandboxPoliciesError::PartialSplitPolicies
        );
    }
    std::assert_matches!(
        resolve_sandbox_policies(
            Path::new("/tmp"),
            Some(SandboxPolicy::new_read_only_policy()),
            Some(VfsPolicy::unrestricted()),
            Some(SocketPolicy::Enabled),
        ),
        Err(ResolveSandboxPoliciesError::MismatchedSandboxPolicy { .. })
    );
    assert_eq!(
        resolve_sandbox_policies(Path::new("/tmp"), None, None, None)
            .expect_err("missing configuration"),
        ResolveSandboxPoliciesError::MissingConfiguration
    );
}
