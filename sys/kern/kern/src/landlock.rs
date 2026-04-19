use crate::protocol::SandboxPolicy;
use crate::spawn::SpawnChildRequest;
use crate::spawn::StdioPolicy;
use crate::spawn::spawn_child_async;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsPolicy;
use chaos_parole::sandbox::vfs_policy_from_sandbox_policy;
use chaos_pf::NetworkProxy;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use tokio::process::Child;

/// Spawn a shell tool command under the Linux sandbox helper
/// (alcatraz-linux), which uses Landlock for filesystem isolation plus
/// seccomp for network restrictions.
///
/// Unlike macOS Seatbelt where we directly embed the policy text, the Linux
/// helper is a separate executable. We pass the legacy [`SandboxPolicy`] plus
/// split filesystem/network policies as JSON so the helper can migrate
/// incrementally without breaking older call sites.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_command_under_linux_sandbox<P>(
    alcatraz_linux_exe: P,
    command: Vec<String>,
    command_cwd: PathBuf,
    sandbox_policy: &SandboxPolicy,
    sandbox_policy_cwd: &Path,
    stdio_policy: StdioPolicy,
    network: Option<&NetworkProxy>,
    env: HashMap<String, String>,
) -> std::io::Result<Child>
where
    P: AsRef<Path>,
{
    let vfs_policy = vfs_policy_from_sandbox_policy(sandbox_policy, sandbox_policy_cwd);
    let socket_policy = SocketPolicy::from(sandbox_policy);
    let args = create_linux_sandbox_command_args_for_policies(
        command,
        sandbox_policy,
        &vfs_policy,
        socket_policy,
        sandbox_policy_cwd,
        allow_network_for_proxy(/*enforce_managed_network*/ false),
    );
    let arg0 = Some("alcatraz-linux");
    spawn_child_async(SpawnChildRequest {
        program: alcatraz_linux_exe.as_ref().to_path_buf(),
        args,
        arg0,
        cwd: command_cwd,
        socket_policy,
        network,
        stdio_policy,
        env,
    })
    .await
}

pub(crate) fn allow_network_for_proxy(enforce_managed_network: bool) -> bool {
    // When managed network requirements are active, request proxy-only
    // networking from the Linux sandbox helper. Without managed requirements,
    // preserve existing behavior.
    enforce_managed_network
}

/// Converts the sandbox policies into the CLI invocation for
/// `alcatraz-linux`.
///
/// The helper performs the actual sandboxing (Landlock + seccomp) after parsing
/// these arguments. Policy JSON flags are emitted before helper feature flags
/// so the argv order matches the helper's CLI shape. See `docs/linux_sandbox.md`
/// for the Linux semantics.
pub(crate) fn create_linux_sandbox_command_args_for_policies(
    command: Vec<String>,
    sandbox_policy: &SandboxPolicy,
    vfs_policy: &VfsPolicy,
    socket_policy: SocketPolicy,
    sandbox_policy_cwd: &Path,
    allow_network_for_proxy: bool,
) -> Vec<String> {
    let sandbox_policy_json = serde_json::to_string(sandbox_policy)
        .unwrap_or_else(|err| panic!("failed to serialize sandbox policy: {err}"));
    let file_system_policy_json = serde_json::to_string(vfs_policy)
        .unwrap_or_else(|err| panic!("failed to serialize filesystem sandbox policy: {err}"));
    let network_policy_json = serde_json::to_string(&socket_policy)
        .unwrap_or_else(|err| panic!("failed to serialize network sandbox policy: {err}"));
    let sandbox_policy_cwd = sandbox_policy_cwd
        .to_str()
        .unwrap_or_else(|| panic!("cwd must be valid UTF-8"))
        .to_string();

    let mut linux_cmd: Vec<String> = vec![
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
        linux_cmd.push("--allow-network-for-proxy".to_string());
    }
    linux_cmd.push("--".to_string());
    linux_cmd.extend(command);
    linux_cmd
}

/// Converts the sandbox cwd and execution options into the CLI invocation for
/// `alcatraz-linux`.
#[cfg(test)]
pub(crate) fn create_linux_sandbox_command_args(
    command: Vec<String>,
    sandbox_policy_cwd: &Path,
    allow_network_for_proxy: bool,
) -> Vec<String> {
    let sandbox_policy_cwd = sandbox_policy_cwd
        .to_str()
        .unwrap_or_else(|| panic!("cwd must be valid UTF-8"))
        .to_string();

    let mut linux_cmd: Vec<String> = vec!["--sandbox-policy-cwd".to_string(), sandbox_policy_cwd];
    if allow_network_for_proxy {
        linux_cmd.push("--allow-network-for-proxy".to_string());
    }

    // Separator so that command arguments starting with `-` are not parsed as
    // options of the helper itself.
    linux_cmd.push("--".to_string());

    // Append the original tool command.
    linux_cmd.extend(command);

    linux_cmd
}

#[cfg(test)]
#[path = "landlock_tests.rs"]
mod tests;
