//! FreeBSD sandbox compatibility checks for the generic exec helper.
//!
//! Capsicum capability mode is all-or-nothing: once a process enters it, the
//! global filesystem namespace and new socket creation are both gone. That can
//! work for tightly controlled children that operate entirely on inherited file
//! descriptors, but it does not work for arbitrary commands launched via
//! `alcatraz-freebsd`, which still expect pathname-based `open(2)` and their
//! own socket creation.
//!
//! The current FreeBSD exec helper therefore fails closed for restrictive
//! configurations that it cannot yet enforce correctly instead of entering
//! capability mode and breaking common command behavior.

use alcatraz_base::error::AlcatrazError;
use alcatraz_base::error::Result;
use chaos_ipc::protocol::FileSystemSandboxPolicy;
use chaos_ipc::protocol::NetworkSandboxPolicy;

/// Apply sandbox policies inside this process before exec.
///
/// The generic FreeBSD exec helper only supports policy combinations that do
/// not require entering capability mode. Restrictive filesystem and network
/// modes currently fail fast with descriptive errors so the caller does not get
/// a misleading partially-broken sandbox.
pub fn apply_sandbox_policy_to_current_thread(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
    proxy_routed_network: bool,
) -> Result<()> {
    if let Some(err) = unsupported_exec_helper_policy(
        file_system_sandbox_policy,
        network_sandbox_policy,
        allow_network_for_proxy,
        proxy_routed_network,
    ) {
        return Err(err);
    }

    Ok(())
}

fn unsupported_exec_helper_policy(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
    proxy_routed_network: bool,
) -> Option<AlcatrazError> {
    if proxy_routed_network || allow_network_for_proxy {
        return Some(AlcatrazError::UnsupportedOperation(
            "Managed network proxy mode is not yet supported by the FreeBSD Capsicum exec helper because generic child processes cannot preserve proxy-only connectivity after capability mode removes ordinary socket creation."
                .to_string(),
        ));
    }

    if !file_system_sandbox_policy.has_full_disk_write_access() {
        if !file_system_sandbox_policy.has_full_disk_read_access() {
            return Some(AlcatrazError::UnsupportedOperation(
                "Restricted read-only access is not yet supported by the FreeBSD Capsicum exec helper."
                    .to_string(),
            ));
        }

        return Some(AlcatrazError::UnsupportedOperation(
            "Restricted filesystem access is not yet supported by the FreeBSD Capsicum exec helper because arbitrary child processes cannot consume pre-opened directory file descriptors after capability mode is entered."
                .to_string(),
        ));
    }

    if should_restrict_network(network_sandbox_policy, allow_network_for_proxy) {
        return Some(AlcatrazError::UnsupportedOperation(
            "Network-only sandboxing is not yet supported by the FreeBSD Capsicum exec helper because capability mode would also deny pathname-based filesystem access for ordinary commands."
                .to_string(),
        ));
    }

    None
}

/// Check if network should be restricted based on policy and proxy settings.
fn should_restrict_network(
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
) -> bool {
    // Mirror Linux logic: managed-network sessions remain fail-closed even for
    // policies that would normally grant full access.
    !network_sandbox_policy.is_enabled() || allow_network_for_proxy
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::protocol::NetworkSandboxPolicy;
    use chaos_ipc::protocol::SandboxPolicy;

    #[test]
    fn should_restrict_when_network_disabled() {
        assert!(should_restrict_network(
            NetworkSandboxPolicy::Restricted,
            false,
        ));
    }

    #[test]
    fn should_restrict_when_proxy_mode_even_with_full_network() {
        assert!(should_restrict_network(NetworkSandboxPolicy::Enabled, true));
    }

    #[test]
    fn should_not_restrict_when_full_network_no_proxy() {
        assert!(!should_restrict_network(
            NetworkSandboxPolicy::Enabled,
            false,
        ));
    }

    #[test]
    fn unrestricted_policy_is_supported() {
        assert!(
            unsupported_exec_helper_policy(
                &FileSystemSandboxPolicy::unrestricted(),
                NetworkSandboxPolicy::Enabled,
                false,
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn restricted_read_only_policy_is_rejected() {
        let err = unsupported_exec_helper_policy(
            &FileSystemSandboxPolicy::from(&SandboxPolicy::new_read_only_policy()),
            NetworkSandboxPolicy::Restricted,
            false,
            false,
        )
        .expect("read-only policy should be rejected");

        // new_read_only_policy() has full read access but not full write,
        // so it hits the "Restricted filesystem access" check.
        assert!(
            err.to_string().contains("Restricted filesystem access"),
            "{err}"
        );
    }

    #[test]
    fn restricted_filesystem_policy_is_rejected() {
        let err = unsupported_exec_helper_policy(
            &FileSystemSandboxPolicy::from(&SandboxPolicy::new_workspace_write_policy()),
            NetworkSandboxPolicy::Enabled,
            false,
            false,
        )
        .expect("workspace-write policy should be rejected");

        assert!(
            err.to_string().contains("Restricted filesystem access"),
            "{err}"
        );
    }

    #[test]
    fn network_only_restriction_is_rejected() {
        let err = unsupported_exec_helper_policy(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Restricted,
            false,
            false,
        )
        .expect("network-only restriction should be rejected");

        assert!(err.to_string().contains("Network-only sandboxing"), "{err}");
    }

    #[test]
    fn managed_proxy_mode_is_rejected() {
        let err = unsupported_exec_helper_policy(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Enabled,
            true,
            true,
        )
        .expect("managed proxy mode should be rejected");

        assert!(
            err.to_string().contains("Managed network proxy mode"),
            "{err}"
        );
    }
}
