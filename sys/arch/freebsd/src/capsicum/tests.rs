use super::*;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SocketPolicy;
use std::path::PathBuf;

#[test]
fn prepare_command_serializes_policies_and_command() {
    let sandbox_policy = SandboxPolicy::new_workspace_write_policy();
    let vfs_policy = VfsPolicy::from(&sandbox_policy);
    let socket_policy = SocketPolicy::from(&sandbox_policy);

    let prepared = prepare_command_from_policies(
        PathBuf::from("/usr/local/bin/alcatraz"),
        vec!["/bin/echo".to_string(), "hello".to_string()],
        &sandbox_policy,
        &vfs_policy,
        socket_policy,
        Path::new("/tmp"),
        true,
    )
    .expect("command preparation should succeed");

    assert_eq!(prepared.program, PathBuf::from("/usr/local/bin/alcatraz"));
    assert_eq!(prepared.arg0.as_deref(), Some("alcatraz"));
    assert_eq!(prepared.args[0], "--sandbox-policy-cwd");
    assert_eq!(prepared.args[1], "/tmp");
    assert_eq!(prepared.args[8], "--allow-network-for-proxy");
    assert_eq!(prepared.args[9], "--");
    assert_eq!(prepared.args[10], "/bin/echo");
    assert_eq!(prepared.args[11], "hello");
}

#[test]
fn prepare_command_omits_proxy_flag_when_disabled() {
    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let vfs_policy = VfsPolicy::from(&sandbox_policy);
    let socket_policy = SocketPolicy::from(&sandbox_policy);

    let prepared = prepare_command_from_policies(
        PathBuf::from("/usr/local/bin/alcatraz"),
        vec!["/bin/echo".to_string()],
        &sandbox_policy,
        &vfs_policy,
        socket_policy,
        Path::new("/tmp"),
        false,
    )
    .expect("command preparation should succeed");

    assert!(
        !prepared
            .args
            .contains(&"--allow-network-for-proxy".to_string())
    );
}

#[test]
fn network_restrictions_include_managed_proxies() {
    for (policy, proxy, expected) in [
        (SocketPolicy::Restricted, false, true),
        (SocketPolicy::Enabled, true, true),
        (SocketPolicy::Enabled, false, false),
    ] {
        assert_eq!(should_restrict_network(policy, proxy), expected);
    }
}

#[test]
fn unrestricted_policy_succeeds() {
    let result = apply_sandbox_policy_to_current_thread(
        &VfsPolicy::unrestricted(),
        SocketPolicy::Enabled,
        false,
        false,
    );
    assert!(result.is_ok());
}

#[test]
fn unsupported_restrictions_fail_closed() {
    for (name, vfs, socket, proxy) in [
        (
            "read-only",
            VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
            SocketPolicy::Restricted,
            false,
        ),
        (
            "workspace-write",
            VfsPolicy::from(&SandboxPolicy::new_workspace_write_policy()),
            SocketPolicy::Enabled,
            false,
        ),
        (
            "network-only",
            VfsPolicy::unrestricted(),
            SocketPolicy::Restricted,
            false,
        ),
        (
            "managed proxy",
            VfsPolicy::unrestricted(),
            SocketPolicy::Enabled,
            true,
        ),
    ] {
        assert!(
            apply_sandbox_policy_to_current_thread(&vfs, socket, proxy, proxy).is_err(),
            "{name} restrictions must fail closed"
        );
    }
}
