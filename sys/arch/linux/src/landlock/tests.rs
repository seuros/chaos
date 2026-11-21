use super::NetworkSeccompMode;
use super::network_seccomp_mode;
use super::should_install_network_seccomp;
use chaos_ipc::protocol::SocketPolicy;
use pretty_assertions::assert_eq;

#[test]
fn network_policy_and_managed_proxy_select_seccomp_enforcement() {
    for (policy, managed, routed, expected) in [
        (SocketPolicy::Enabled, false, false, None),
        (
            SocketPolicy::Enabled,
            true,
            false,
            Some(NetworkSeccompMode::Restricted),
        ),
        (
            SocketPolicy::Enabled,
            true,
            true,
            Some(NetworkSeccompMode::ProxyRouted),
        ),
        (
            SocketPolicy::Restricted,
            false,
            false,
            Some(NetworkSeccompMode::Restricted),
        ),
        (
            SocketPolicy::Restricted,
            true,
            false,
            Some(NetworkSeccompMode::Restricted),
        ),
    ] {
        assert_eq!(
            should_install_network_seccomp(policy, managed),
            expected.is_some(),
            "policy={policy:?}, managed={managed}"
        );
        assert_eq!(
            network_seccomp_mode(policy, managed, routed),
            expected,
            "policy={policy:?}, managed={managed}, routed={routed}"
        );
    }
}

#[test]
fn kernel_release_parsing_handles_distribution_and_rc_suffixes() {
    for (release, expected) in [
        ("6.19.8-arch1-1", Some((6, 19))),
        ("6.10-rc1", Some((6, 10))),
        ("6.10.0", Some((6, 10))),
        ("6.9.12", Some((6, 9))),
        ("not-a-kernel", None),
    ] {
        assert_eq!(super::parse_kernel_version(release), expected, "{release}");
    }
}

#[test]
fn every_sandbox_closes_the_kernel_keyring_and_only_restricted_ones_the_network() {
    let keyring = [libc::SYS_keyctl, libc::SYS_add_key, libc::SYS_request_key];
    for network in [
        None,
        Some(NetworkSeccompMode::Restricted),
        Some(NetworkSeccompMode::ProxyRouted),
    ] {
        let rules = super::seccomp_rules(network).expect("rules");
        for nr in keyring {
            assert_eq!(
                rules.get(&nr).map(Vec::len),
                Some(0),
                "{network:?} must deny {nr}"
            );
        }
        assert_eq!(
            rules.contains_key(&libc::SYS_connect),
            network == Some(NetworkSeccompMode::Restricted),
            "{network:?}"
        );
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn every_rule_is_mirrored_onto_the_x32_abi() {
    assert_eq!(
        super::x32_syscall(libc::SYS_keyctl),
        250 | super::X32_SYSCALL_BIT
    );
    assert_eq!(
        super::x32_syscall(libc::SYS_ptrace),
        521 | super::X32_SYSCALL_BIT
    );
    assert_eq!(
        super::x32_syscall(libc::SYS_sendmmsg),
        538 | super::X32_SYSCALL_BIT
    );
    for network in [
        Some(NetworkSeccompMode::Restricted),
        Some(NetworkSeccompMode::ProxyRouted),
    ] {
        let native = super::native_seccomp_rules(network).expect("native");
        let rules = super::seccomp_rules(network).expect("rules");
        for (nr, conditions) in &native {
            let mirrored = rules.get(&super::x32_syscall(*nr)).expect("mirrored");
            assert_eq!(mirrored.len(), conditions.len(), "{network:?} {nr}");
        }
    }
}
