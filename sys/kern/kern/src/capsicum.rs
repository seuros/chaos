use crate::landlock::allow_network_for_proxy;
use crate::landlock::create_linux_sandbox_command_args_for_policies;
use crate::protocol::SandboxPolicy;
use crate::spawn::SpawnChildRequest;
use crate::spawn::StdioPolicy;
use crate::spawn::spawn_child_async;
use chaos_ipc::permissions::FileSystemSandboxPolicy;
use chaos_ipc::permissions::NetworkSandboxPolicy;
use chaos_pf::NetworkProxy;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use tokio::process::Child;

/// Spawn a shell tool command under the FreeBSD sandbox helper
/// (alcatraz-freebsd), which uses Capsicum for capability-based sandboxing.
///
/// The FreeBSD helper accepts the same CLI interface as alcatraz-linux:
/// policy JSON args followed by `--` and the command. The only difference
/// is the arg0 dispatch name.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_command_under_freebsd_sandbox<P>(
    alcatraz_freebsd_exe: P,
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
    let file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(sandbox_policy, sandbox_policy_cwd);
    let network_sandbox_policy = NetworkSandboxPolicy::from(sandbox_policy);
    let args = create_linux_sandbox_command_args_for_policies(
        command,
        sandbox_policy,
        &file_system_sandbox_policy,
        network_sandbox_policy,
        sandbox_policy_cwd,
        allow_network_for_proxy(/*enforce_managed_network*/ false),
    );
    let arg0 = Some("alcatraz-freebsd");
    spawn_child_async(SpawnChildRequest {
        program: alcatraz_freebsd_exe.as_ref().to_path_buf(),
        args,
        arg0,
        cwd: command_cwd,
        network_sandbox_policy,
        network,
        stdio_policy,
        env,
    })
    .await
}
