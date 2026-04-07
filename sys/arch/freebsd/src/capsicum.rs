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
//! jails are currently unsupported for general command execution. When such
//! restrictions are requested, we fail closed and return an explicit error.

use alcatraz_base::error::AlcatrazError;
use alcatraz_base::error::Result;
use chaos_ipc::protocol::FileSystemSandboxPolicy;
use chaos_ipc::protocol::NetworkSandboxPolicy;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_pf::NetworkProxy;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Child;
use tokio::process::Command;

const CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR: &str = "CHAOS_SANDBOX_NETWORK_DISABLED";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub arg0: Option<String>,
}

/// Build the helper command line for `alcatraz-freebsd`.
///
/// The helper mirrors the Linux sandbox helper CLI: policy JSON args first,
/// then `--`, then the target command.
#[allow(clippy::too_many_arguments)]
pub fn prepare_command<P>(
    executable: P,
    command: Vec<String>,
    sandbox_policy: &SandboxPolicy,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
    sandbox_policy_cwd: &Path,
    allow_network_for_proxy: bool,
) -> PreparedCommand
where
    P: AsRef<Path>,
{
    let sandbox_policy_json = serde_json::to_string(sandbox_policy)
        .unwrap_or_else(|err| panic!("failed to serialize sandbox policy: {err}"));
    let file_system_policy_json = serde_json::to_string(file_system_sandbox_policy)
        .unwrap_or_else(|err| panic!("failed to serialize filesystem sandbox policy: {err}"));
    let network_policy_json = serde_json::to_string(&network_sandbox_policy)
        .unwrap_or_else(|err| panic!("failed to serialize network sandbox policy: {err}"));
    let sandbox_policy_cwd = sandbox_policy_cwd
        .to_str()
        .unwrap_or_else(|| panic!("cwd must be valid UTF-8"))
        .to_string();

    let mut args = vec![
        "--sandbox-policy-cwd".to_string(),
        sandbox_policy_cwd,
        "--sandbox-policy".to_string(),
        sandbox_policy_json,
        "--file-system-sandbox-policy".to_string(),
        file_system_policy_json,
        "--network-sandbox-policy".to_string(),
        network_policy_json,
    ];
    if allow_network_for_proxy {
        args.push("--allow-network-for-proxy".to_string());
    }
    args.push("--".to_string());
    args.extend(command);

    PreparedCommand {
        program: executable.as_ref().to_path_buf(),
        args,
        arg0: Some("alcatraz-freebsd".to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn spawn_command<P>(
    executable: P,
    command: Vec<String>,
    command_cwd: PathBuf,
    sandbox_policy: &SandboxPolicy,
    sandbox_policy_cwd: &Path,
    network: Option<&NetworkProxy>,
    env: HashMap<String, String>,
) -> std::io::Result<Child>
where
    P: AsRef<Path>,
{
    let file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(sandbox_policy, sandbox_policy_cwd);
    let network_sandbox_policy = NetworkSandboxPolicy::from(sandbox_policy);
    let prepared = prepare_command(
        executable,
        command,
        sandbox_policy,
        &file_system_sandbox_policy,
        network_sandbox_policy,
        sandbox_policy_cwd,
        false,
    );

    let mut env = env;
    if let Some(network) = network {
        network.apply_to_env(&mut env);
    }

    let mut cmd = Command::new(&prepared.program);
    #[cfg(unix)]
    cmd.arg0(
        prepared
            .arg0
            .clone()
            .unwrap_or_else(|| prepared.program.to_string_lossy().to_string()),
    );
    cmd.args(&prepared.args);
    cmd.current_dir(command_cwd);
    cmd.env_clear();
    cmd.envs(env);
    if !network_sandbox_policy.is_enabled() {
        cmd.env(CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR, "1");
    }
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    cmd.kill_on_drop(true).spawn()
}

/// Apply sandbox policies inside this process before exec.
///
/// Always applies process hardening (no-new-privs, anti-ptrace). Logs
/// warnings for enforcement dimensions that are not yet implemented and then
/// rejects execution to avoid fail-open behavior.
pub fn apply_sandbox_policy_to_current_thread(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
    proxy_routed_network: bool,
) -> Result<()> {
    // ── Layer 1: process hardening (always applied) ──────────────────────
    apply_procctl_hardening();
    let mut unsupported_restrictions = Vec::new();

    // ── Layer 2: network isolation ───────────────────────────────────────
    if should_restrict_network(network_sandbox_policy, allow_network_for_proxy) {
        unsupported_restrictions.push(
            "network isolation requested but ipfw enforcement is not yet implemented on FreeBSD",
        );
    }

    if proxy_routed_network || allow_network_for_proxy {
        unsupported_restrictions
            .push("managed network proxy mode requested but not yet implemented on FreeBSD");
    }

    // ── Layer 3: filesystem confinement ──────────────────────────────────
    if !file_system_sandbox_policy.has_full_disk_write_access() {
        if !file_system_sandbox_policy.has_full_disk_read_access() {
            unsupported_restrictions.push(
                "restricted read-only filesystem access requested but jail-based confinement is not yet implemented on FreeBSD",
            );
        } else {
            unsupported_restrictions.push(
                "restricted filesystem write access requested but jail-based confinement is not yet implemented on FreeBSD",
            );
        }
    }

    if unsupported_restrictions.is_empty() {
        return Ok(());
    }

    for message in &unsupported_restrictions {
        eprintln!("alcatraz-freebsd: warning: {message}");
    }

    Err(AlcatrazError::UnsupportedOperation(format!(
        "requested sandbox restrictions are not enforceable on FreeBSD: {}",
        unsupported_restrictions.join("; ")
    )))
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
    use std::path::PathBuf;

    #[test]
    fn prepare_command_serializes_policies_and_command() {
        let sandbox_policy = SandboxPolicy::new_workspace_write_policy();
        let file_system_sandbox_policy = FileSystemSandboxPolicy::from(&sandbox_policy);
        let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);

        let prepared = prepare_command(
            PathBuf::from("/usr/local/bin/alcatraz-freebsd"),
            vec!["/bin/echo".to_string(), "hello".to_string()],
            &sandbox_policy,
            &file_system_sandbox_policy,
            network_sandbox_policy,
            Path::new("/tmp"),
            true,
        );

        assert_eq!(
            prepared.program,
            PathBuf::from("/usr/local/bin/alcatraz-freebsd")
        );
        assert_eq!(prepared.arg0.as_deref(), Some("alcatraz-freebsd"));
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
        let file_system_sandbox_policy = FileSystemSandboxPolicy::from(&sandbox_policy);
        let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);

        let prepared = prepare_command(
            PathBuf::from("/usr/local/bin/alcatraz-freebsd"),
            vec!["/bin/echo".to_string()],
            &sandbox_policy,
            &file_system_sandbox_policy,
            network_sandbox_policy,
            Path::new("/tmp"),
            false,
        );

        assert!(
            !prepared
                .args
                .contains(&"--allow-network-for-proxy".to_string())
        );
    }

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
    fn restricted_read_only_policy_is_rejected() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::from(&SandboxPolicy::new_read_only_policy()),
            NetworkSandboxPolicy::Restricted,
            false,
            false,
        );
        assert!(result.is_err(), "restricted policies must fail closed");
    }

    #[test]
    fn restricted_filesystem_policy_is_rejected() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::from(&SandboxPolicy::new_workspace_write_policy()),
            NetworkSandboxPolicy::Enabled,
            false,
            false,
        );
        assert!(result.is_err(), "workspace-write policy must fail closed");
    }

    #[test]
    fn network_only_restriction_is_rejected() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Restricted,
            false,
            false,
        );
        assert!(result.is_err(), "network-only restriction must fail closed");
    }

    #[test]
    fn managed_proxy_mode_is_rejected() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Enabled,
            true,
            true,
        );
        assert!(result.is_err(), "managed proxy mode must fail closed");
    }

    #[test]
    fn root_access_applies_hardening() {
        let result = apply_sandbox_policy_to_current_thread(
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Enabled,
            false,
            false,
        );
        assert!(
            result.is_ok(),
            "RootAccess should succeed with procctl hardening"
        );
    }
}
