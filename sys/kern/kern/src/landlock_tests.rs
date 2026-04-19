use super::*;
use pretty_assertions::assert_eq;

#[test]
fn use_legacy_landlock_flag_is_no_longer_emitted() {
    let command = vec!["/bin/true".to_string()];
    let cwd = Path::new("/tmp");

    let args = create_linux_sandbox_command_args(command, cwd, false);
    assert_eq!(args.contains(&"--use-legacy-landlock".to_string()), false);
}

#[test]
fn proxy_flag_is_included_when_requested() {
    let command = vec!["/bin/true".to_string()];
    let cwd = Path::new("/tmp");

    let args = create_linux_sandbox_command_args(command, cwd, true);
    assert_eq!(
        args.contains(&"--allow-network-for-proxy".to_string()),
        true
    );
}

#[test]
fn split_policy_flags_are_included() {
    let command = vec!["/bin/true".to_string()];
    let cwd = Path::new("/tmp");
    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let vfs_policy = VfsPolicy::from(&sandbox_policy);
    let socket_policy = SocketPolicy::from(&sandbox_policy);

    let args = create_linux_sandbox_command_args_for_policies(
        command,
        &sandbox_policy,
        &vfs_policy,
        socket_policy,
        cwd,
        false,
    );

    assert_eq!(
        args.windows(2)
            .any(|window| { window[0] == "--file-system-sandbox-policy" && !window[1].is_empty() }),
        true
    );
    assert_eq!(
        args.windows(2)
            .any(|window| window[0] == "--network-sandbox-policy" && window[1] == "\"restricted\""),
        true
    );
}

#[test]
fn proxy_network_requires_managed_requirements() {
    assert_eq!(allow_network_for_proxy(false), false);
    assert_eq!(allow_network_for_proxy(true), true);
}
