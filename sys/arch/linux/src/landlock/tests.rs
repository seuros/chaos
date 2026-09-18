use super::NetworkSeccompMode;
use super::network_seccomp_mode;
use super::should_install_network_seccomp;
use chaos_ipc::protocol::SocketPolicy;
use pretty_assertions::assert_eq;

#[test]
fn managed_network_enforces_seccomp_even_for_full_network_policy() {
    assert_eq!(
        should_install_network_seccomp(SocketPolicy::Enabled, true),
        true
    );
}

#[test]
fn full_network_policy_without_managed_network_skips_seccomp() {
    assert_eq!(
        should_install_network_seccomp(SocketPolicy::Enabled, false),
        false
    );
}

#[test]
fn restricted_network_policy_always_installs_seccomp() {
    assert!(should_install_network_seccomp(
        SocketPolicy::Restricted,
        false
    ));
    assert!(should_install_network_seccomp(
        SocketPolicy::Restricted,
        true
    ));
}

#[test]
fn managed_proxy_routes_use_proxy_routed_seccomp_mode() {
    assert_eq!(
        network_seccomp_mode(SocketPolicy::Enabled, true, true),
        Some(NetworkSeccompMode::ProxyRouted)
    );
}

#[test]
fn restricted_network_without_proxy_routing_uses_restricted_mode() {
    assert_eq!(
        network_seccomp_mode(SocketPolicy::Restricted, false, false),
        Some(NetworkSeccompMode::Restricted)
    );
}

#[test]
fn full_network_without_managed_proxy_skips_network_seccomp_mode() {
    assert_eq!(
        network_seccomp_mode(SocketPolicy::Enabled, false, false),
        None
    );
}

#[test]
fn parse_kernel_version_standard() {
    assert_eq!(super::parse_kernel_version("6.19.8-arch1-1"), Some((6, 19)));
}

#[test]
fn parse_kernel_version_release_candidate() {
    assert_eq!(super::parse_kernel_version("6.10-rc1"), Some((6, 10)));
}

#[test]
fn parse_kernel_version_minimum_accepted() {
    assert_eq!(super::parse_kernel_version("6.10.0"), Some((6, 10)));
}

#[test]
fn parse_kernel_version_too_old() {
    assert_eq!(super::parse_kernel_version("6.9.12"), Some((6, 9)));
}

#[test]
fn parse_kernel_version_garbage() {
    assert_eq!(super::parse_kernel_version("not-a-kernel"), None);
}

#[test]
fn check_minimum_kernel_passes_on_current_host() {
    // This test runs on the CI/dev machine — it must be ≥ 6.10.
    super::check_minimum_kernel_version().expect("host kernel should be ≥ 6.10");
}
