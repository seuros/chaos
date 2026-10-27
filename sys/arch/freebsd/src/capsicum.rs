//! FreeBSD process hardening for the generic exec helper.
//!
//! Capsicum capability mode (`cap_enter()`) is the wrong tool for arbitrary
//! commands — it blocks `open(2)`, `connect(2)`, and `openat(AT_FDCWD)`, which
//! ordinary shell commands rely on. This module retains its historical name
//! but does not claim Capsicum capability-mode confinement.
//!
//! What we *can* do for every child process right now:
//!
//!   1. `procctl(PROC_NO_NEW_PRIVS_CTL)` — prevent setuid/setgid escalation.
//!   2. `procctl(PROC_TRACE_CTL, PROC_TRACE_CTL_DISABLE)` — block ptrace.
//!
//! mac_gulag ABI 1 provides network/process restrictions only. Filesystem
//! restrictions require a private Warden jail/nullfs connection, otherwise
//! they fail closed. Unsupported policies/modules also fail closed.

use alcatraz_base::error::AlcatrazError;
use alcatraz_base::error::Result;
use alcatraz_base::prepared_command::PreparedCommand;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SocketPolicy;
use chaos_ipc::protocol::VfsPolicy;
use chaos_parole::sandbox::vfs_policy_from_sandbox_policy;
use chaos_parole::sandbox::{has_full_disk_read_access, has_full_disk_write_access};
use chaos_pf::NetworkProxy;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Child;
use tokio::process::Command;

const CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR: &str = "CHAOS_SANDBOX_NETWORK_DISABLED";

/// Build the helper command line for `alcatraz`.
///
/// The helper mirrors the Linux sandbox helper CLI: policy JSON args first,
/// then `--`, then the target command.
#[allow(clippy::too_many_arguments)]
pub fn prepare_command_from_policies<P>(
    executable: P,
    command: Vec<String>,
    sandbox_policy: &SandboxPolicy,
    vfs_policy: &VfsPolicy,
    socket_policy: SocketPolicy,
    sandbox_policy_cwd: &Path,
    allow_network_for_proxy: bool,
) -> std::io::Result<PreparedCommand>
where
    P: AsRef<Path>,
{
    let sandbox_policy_json =
        serde_json::to_string(sandbox_policy).map_err(std::io::Error::other)?;
    let file_system_policy_json =
        serde_json::to_string(vfs_policy).map_err(std::io::Error::other)?;
    let network_policy_json =
        serde_json::to_string(&socket_policy).map_err(std::io::Error::other)?;
    let sandbox_policy_cwd = sandbox_policy_cwd
        .to_str()
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "cwd must be valid UTF-8")
        })?
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

    let mut env = Default::default();
    if !has_full_disk_write_access(vfs_policy)
        && let Some(fd) = crate::warden_client::prepared_environment()
    {
        env = std::collections::HashMap::from([(crate::warden_client::CONTROL_ENV.to_owned(), fd)]);
    }
    Ok(PreparedCommand {
        program: executable.as_ref().to_path_buf(),
        args,
        env,
        arg0: Some("alcatraz".to_string()),
    })
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
    let vfs_policy = vfs_policy_from_sandbox_policy(sandbox_policy, sandbox_policy_cwd);
    let socket_policy = SocketPolicy::from(sandbox_policy);
    let prepared = prepare_command_from_policies(
        executable,
        command,
        sandbox_policy,
        &vfs_policy,
        socket_policy,
        sandbox_policy_cwd,
        false,
    )?;

    let mut env = env;
    env.extend(prepared.env.clone());
    if let Some(network) = network {
        network.apply_to_env(&mut env);
    }

    let mut cmd = Command::new(&prepared.program);
    crate::warden_client::inherit_for_helper(
        cmd.as_std_mut(),
        prepared.arg0.as_deref(),
        env.get(crate::warden_client::CONTROL_ENV)
            .map(String::as_str),
    )?;
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
    if !socket_policy.is_enabled() {
        cmd.env(CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR, "1");
    }
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    cmd.kill_on_drop(true).spawn()
}

/// Apply sandbox policies inside this process before exec.
///
/// Requires successful process hardening (no-new-privs, anti-ptrace).
/// Unsupported policies and unavailable MAC enforcement fail closed.
pub fn apply_sandbox_policy_to_current_thread(
    vfs_policy: &VfsPolicy,
    socket_policy: SocketPolicy,
    allow_network_for_proxy: bool,
    proxy_routed_network: bool,
    sandbox_policy_cwd: &Path,
) -> Result<()> {
    // ── Layer 1: process hardening (always applied) ──────────────────────
    apply_procctl_hardening()?;

    if proxy_routed_network || allow_network_for_proxy {
        return Err(AlcatrazError::UnsupportedOperation(
            "managed network proxy mode is not supported by mac_gulag ABI 1".into(),
        ));
    }

    if has_full_disk_read_access(vfs_policy)
        && has_full_disk_write_access(vfs_policy)
        && !should_restrict_network(socket_policy, false)
    {
        return Ok(());
    }
    crate::gulag::apply(vfs_policy, socket_policy, sandbox_policy_cwd)
}

/// Apply `procctl` process hardening that is safe for arbitrary commands.
///
/// - `PROC_NO_NEW_PRIVS_CTL`: prevents setuid/setgid privilege escalation.
///   Survives `execve()`.
/// - `PROC_TRACE_CTL`: disables `ptrace(2)` attachment to this process.
///   Survives `execve()` with `PROC_TRACE_CTL_DISABLE_EXEC`.
fn apply_procctl_hardening() -> Result<()> {
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
        return Err(std::io::Error::last_os_error().into());
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
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Check if network should be restricted based on policy and proxy settings.
fn should_restrict_network(socket_policy: SocketPolicy, allow_network_for_proxy: bool) -> bool {
    !socket_policy.is_enabled() || allow_network_for_proxy
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
