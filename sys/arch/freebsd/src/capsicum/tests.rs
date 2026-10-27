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
fn network_restriction_truth_table() {
    for (network, proxy, restricted) in [
        (SocketPolicy::Enabled, false, false),
        (SocketPolicy::Enabled, true, true),
        (SocketPolicy::Restricted, false, true),
        (SocketPolicy::Restricted, true, true),
    ] {
        assert_eq!(should_restrict_network(network, proxy), restricted);
    }
}

#[test]
fn managed_proxy_mode_is_rejected() {
    let result = apply_sandbox_policy_to_current_thread(
        &VfsPolicy::unrestricted(),
        SocketPolicy::Enabled,
        true,
        true,
        Path::new("/tmp"),
    );
    assert!(result.is_err(), "managed proxy mode must fail closed");
}

#[test]
fn root_access_applies_hardening() {
    let result = apply_sandbox_policy_to_current_thread(
        &VfsPolicy::unrestricted(),
        SocketPolicy::Enabled,
        false,
        false,
        Path::new("/tmp"),
    );
    assert!(
        result.is_ok(),
        "RootAccess should succeed with procctl hardening"
    );
    let mut status = 0;
    assert_eq!(
        unsafe {
            libc::procctl(
                libc::P_PID,
                0,
                libc::PROC_NO_NEW_PRIVS_STATUS,
                std::ptr::addr_of_mut!(status).cast(),
            )
        },
        0
    );
    assert_eq!(status, libc::PROC_NO_NEW_PRIVS_ENABLE);
}
