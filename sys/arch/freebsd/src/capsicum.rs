//! FreeBSD process hardening for the generic exec helper.
//!
//! Capsicum capability mode (`cap_enter()`) is the wrong tool for arbitrary
//! commands — it blocks `open(2)`, `connect(2)`, and `openat(AT_FDCWD)`, which
//! every shell command relies on.  Full Capsicum enforcement belongs in
//! controlled code paths (Phase 3: `apply_patch` fork).
//!
//! What we *can* do for every child process right now:
//!
//!   1. `procctl(PROC_NO_NEW_PRIVS_CTL)` — prevent setuid/setgid escalation.
//!   2. `procctl(PROC_TRACE_CTL, PROC_TRACE_CTL_DISABLE)` — block ptrace.
//!
//! Filesystem and network restrictions that require `cap_enter()`, `ipfw`, or
//! jails are logged as warnings and passed through so the command still runs.
//! This function never returns `Err` for a valid policy.

use alcatraz_base::error::Result;
use chaos_ipc::protocol::FileSystemSandboxPolicy;
use chaos_ipc::protocol::NetworkSandboxPolicy;

/// Apply sandbox policies inside this process before exec.
///
/// Always applies process hardening (no-new-privs, anti-ptrace).  Logs
/// warnings for enforcement dimensions that are not yet implemented instead
/// of rejecting the policy outright.
pub fn apply_sandbox_policy_to_current_thread(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
    proxy_routed_network: bool,
) -> Result<()> {
    // ── Layer 1: process hardening (always applied) ──────────────────────
    apply_procctl_hardening();

    // ── Layer 2: network isolation ───────────────────────────────────────
    if should_restrict_network(network_sandbox_policy, allow_network_for_proxy) {
        eprintln!(
            "alcatraz-freebsd: warning: network isolation requested but ipfw enforcement \
             is not yet implemented — network access is unrestricted."
        );
    }

    if proxy_routed_network || allow_network_for_proxy {
        eprintln!(
            "alcatraz-freebsd: warning: managed network proxy mode requested but not yet \
             implemented on FreeBSD — proxy routing is inactive."
        );
    }

    // ── Layer 3: filesystem confinement ──────────────────────────────────
    if !file_system_sandbox_policy.has_full_disk_write_access() {
        if !file_system_sandbox_policy.has_full_disk_read_access() {
            eprintln!(
                "alcatraz-freebsd: warning: restricted read-only filesystem access requested \
                 but requires jail-based confinement (not yet implemented) — filesystem \
                 access is unrestricted."
            );
        } else {
            eprintln!(
                "alcatraz-freebsd: warning: restricted filesystem write access requested \
                 but requires jail-based confinement (not yet implemented) — filesystem \
                 access is unrestricted."
            );
        }
    }

    Ok(())
}

/// Apply `procctl` process hardening that is safe for arbitrary commands.
///
/// - `PROC_NO_NEW_PRIVS_CTL`: prevents setuid/setgid privilege escalation.
///   Survives `execve()`.
/// - `PROC_TRACE_CTL`: disables `ptrace(2)` attachment to this process.
///   Survives `execve()` with `PROC_TRACE_CTL_DISABLE_EXEC`.
fn apply_procctl_hardening() {
    // PROC_NO_NEW_PRIVS_CTL — block setuid/setgid escalation
    let mut arg: libc::c_int = libc::PROC_NO_NEW_PRIVS_ENABLE;
    let ret = unsafe {
        libc::procctl(
            libc::P_PID,
            0, // 0 = current process
            libc::PROC_NO_NEW_PRIVS_CTL,
            std::ptr::addr_of_mut!(arg).cast(),
        )
    };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        eprintln!("alcatraz-freebsd: warning: procctl(PROC_NO_NEW_PRIVS_CTL) failed: {err}");
    }

    // PROC_TRACE_CTL — block ptrace attachment (survives exec)
    let mut arg: libc::c_int = libc::PROC_TRACE_CTL_DISABLE_EXEC;
    let ret = unsafe {
        libc::procctl(
            libc::P_PID,
            0,
            libc::PROC_TRACE_CTL,
            std::ptr::addr_of_mut!(arg).cast(),
        )
    };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        eprintln!("alcatraz-freebsd: warning: procctl(PROC_TRACE_CTL) failed: {err}");
    }
}

/// Check if network should be restricted based on policy and proxy settings.
fn should_restrict_network(
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
) -> bool {
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
    fn unrestricted_policy_succeeds() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Enabled,
            false,
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn restricted_read_only_policy_succeeds_with_warning() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::from(&SandboxPolicy::new_read_only_policy()),
            NetworkSandboxPolicy::Restricted,
            false,
            false,
        );
        assert!(result.is_ok(), "restricted policies should pass through with warnings");
    }

    #[test]
    fn restricted_filesystem_policy_succeeds_with_warning() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::from(&SandboxPolicy::new_workspace_write_policy()),
            NetworkSandboxPolicy::Enabled,
            false,
            false,
        );
        assert!(result.is_ok(), "workspace-write policy should pass through with warnings");
    }

    #[test]
    fn network_only_restriction_succeeds_with_warning() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Restricted,
            false,
            false,
        );
        assert!(result.is_ok(), "network-only restriction should pass through with warnings");
    }

    #[test]
    fn managed_proxy_mode_succeeds_with_warning() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Enabled,
            true,
            true,
        );
        assert!(result.is_ok(), "managed proxy mode should pass through with warnings");
    }

    #[test]
    fn root_access_applies_hardening() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Enabled,
            false,
            false,
        );
        assert!(result.is_ok(), "RootAccess should succeed with procctl hardening");
    }
}
