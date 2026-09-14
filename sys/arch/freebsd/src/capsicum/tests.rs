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
fn should_restrict_when_network_disabled() {
    assert!(should_restrict_network(SocketPolicy::Restricted, false,));
}

#[test]
fn should_restrict_when_proxy_mode_even_with_full_network() {
    assert!(should_restrict_network(SocketPolicy::Enabled, true));
}

#[test]
fn should_not_restrict_when_full_network_no_proxy() {
    assert!(!should_restrict_network(SocketPolicy::Enabled, false,));
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
fn restricted_read_only_policy_is_rejected() {
    let result = apply_sandbox_policy_to_current_thread(
        &VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
        SocketPolicy::Restricted,
        false,
        false,
    );
    assert!(result.is_err(), "restricted policies must fail closed");
}

#[test]
fn restricted_filesystem_policy_is_rejected() {
    let result = apply_sandbox_policy_to_current_thread(
        &VfsPolicy::from(&SandboxPolicy::new_workspace_write_policy()),
        SocketPolicy::Enabled,
        false,
        false,
    );
    assert!(result.is_err(), "workspace-write policy must fail closed");
}

#[test]
fn network_only_restriction_is_rejected() {
    let result = apply_sandbox_policy_to_current_thread(
        &VfsPolicy::unrestricted(),
        SocketPolicy::Restricted,
        false,
        false,
    );
    assert!(result.is_err(), "network-only restriction must fail closed");
}

#[test]
fn managed_proxy_mode_is_rejected() {
    let result = apply_sandbox_policy_to_current_thread(
        &VfsPolicy::unrestricted(),
        SocketPolicy::Enabled,
        true,
        true,
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
    );
    assert!(
        result.is_ok(),
        "RootAccess should succeed with procctl hardening"
    );
}
