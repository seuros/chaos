//! Sandbox and egress wiring for clamp subprocesses.
//!
//! The clamp crate owns the CONNECT proxy and the destination allowlist; this
//! module owns the kernel-side half that makes the allowlist binding. Landlock
//! network rules are port-scoped rather than host-scoped, so confinement alone
//! cannot express "only these three hosts". The two halves compose: the sandbox
//! helper permits exactly the loopback port the proxy bound, and the proxy
//! decides which upstream hosts that port will reach. A subprocess that ignores
//! the proxy environment reaches nothing at all.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use chaos_clamp::AntigravityEgress;
use chaos_clamp::AntigravitySandbox;
use chaos_clamp::EgressPolicy;
use chaos_clamp::EgressProxy;
use chaos_clamp::WiretapSink;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsAccessMode;
use chaos_ipc::permissions::VfsEntry;
use chaos_ipc::permissions::VfsPath;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::permissions::VfsSpecialPath;
use chaos_realpath::AbsolutePathBuf;

/// The helper is the multicall Chaos executable, which selects the sandbox tool
/// from `argv[0]` rather than a subcommand.
const LINUX_SANDBOX_ARG0: &str = "alcatraz";

/// Starts the Antigravity egress proxy and returns the environment the CLI
/// needs to route through it, along with the handle keeping it alive.
pub async fn start_antigravity_egress(
    sink: Arc<dyn WiretapSink>,
    ca_bundle_path: PathBuf,
) -> Result<(EgressProxy, AntigravityEgress), String> {
    let proxy = EgressProxy::start(EgressPolicy::antigravity(), sink, Some(ca_bundle_path))
        .await
        .map_err(|error| format!("failed to start Antigravity egress proxy: {error}"))?;
    let egress = AntigravityEgress {
        proxy_url: proxy.proxy_url(),
        ca_bundle_path: proxy.ca_bundle_path().map(Path::to_path_buf),
    };
    Ok((proxy, egress))
}

/// Builds the sandbox helper invocation that wraps the Antigravity CLI.
///
/// The CLI reads its own installation tree and writes only its dedicated home
/// and working directory. Networking is permitted solely so the helper can
/// derive the proxy loopback port from `HTTPS_PROXY`; every other destination
/// is refused before a connection exists.
pub fn antigravity_sandbox(
    helper: &Path,
    home: Option<&Path>,
    cwd: &Path,
) -> Result<AntigravitySandbox, String> {
    let mut entries = vec![VfsEntry {
        path: VfsPath::Special {
            value: VfsSpecialPath::Root,
        },
        access: VfsAccessMode::Read,
    }];
    for writable in [Some(cwd), home].into_iter().flatten() {
        let path = AbsolutePathBuf::from_absolute_path(writable).map_err(|error| {
            format!("Antigravity sandbox path {writable:?} is unusable: {error}")
        })?;
        entries.push(VfsEntry {
            path: VfsPath::Path { path },
            access: VfsAccessMode::Write,
        });
    }
    entries.push(VfsEntry {
        path: VfsPath::Special {
            value: VfsSpecialPath::Tmpdir,
        },
        access: VfsAccessMode::Write,
    });

    let vfs_policy = VfsPolicy::restricted(entries);
    let sandbox_policy = vfs_policy
        .to_sandbox_policy(SocketPolicy::Enabled, cwd)
        .map_err(|error| format!("failed to project Antigravity sandbox policy: {error}"))?;
    let args = crate::landlock::create_linux_sandbox_command_args_for_policies(
        Vec::new(),
        &sandbox_policy,
        &vfs_policy,
        SocketPolicy::Enabled,
        cwd,
        /* allow_network_for_proxy */ true,
    );
    Ok(AntigravitySandbox {
        program: helper.to_path_buf(),
        arg0: Some(LINUX_SANDBOX_ARG0.to_string()),
        args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_args_confine_writes_and_request_proxy_only_networking() {
        let sandbox = antigravity_sandbox(
            Path::new("/usr/lib/chaos/alcatraz"),
            Some(Path::new("/private/agy")),
            Path::new("/srv/work"),
        )
        .expect("build sandbox invocation");

        assert_eq!(sandbox.program, Path::new("/usr/lib/chaos/alcatraz"));
        assert_eq!(sandbox.args.last().map(String::as_str), Some("--"));
        assert!(
            sandbox
                .args
                .iter()
                .any(|arg| arg == "--allow-network-for-proxy")
        );
        let file_system_policy = sandbox
            .args
            .iter()
            .position(|arg| arg == "--file-system-sandbox-policy")
            .and_then(|index| sandbox.args.get(index + 1))
            .expect("filesystem policy argument");
        assert!(file_system_policy.contains("/private/agy"));
        assert!(file_system_policy.contains("/srv/work"));
    }
}
