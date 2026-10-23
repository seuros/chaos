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
