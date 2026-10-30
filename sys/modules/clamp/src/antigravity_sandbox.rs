//! Platform-native confinement for the subscription-backed agent.
//!
//! A proxy environment is not confinement. The generated OS policy allows the
//! exact proxy endpoint and, on macOS, the session bridge's Unix socket only.
//! In particular it does not grant arbitrary loopback traffic or local binding.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chaos_ipc::permissions::{
    SocketPolicy, VfsAccessMode, VfsEntry, VfsPath, VfsPolicy, VfsSpecialPath,
};
use chaos_pf::{
    ConfigReloader, ConfigState, NetworkProxy, NetworkProxyConfig, NetworkProxyConstraints,
    NetworkProxyState, build_config_state,
};
use chaos_realpath::AbsolutePathBuf;
use rama::futures::future::BoxFuture;

use crate::{AntigravityEgress, AntigravitySandbox};

/// Build the host platform's sandbox invocation, never another OS's arguments.
///
/// When protecting policy, the profile root is read-only; only explicitly
/// listed state subdirectories are writable. Merely carving a read-only hook
/// file out of a writable profile allows its ancestors to be renamed.
/// The caller keeps these paths and the proxy alive for the agent's lifetime.
/// No listener is started by the network-policy descriptor.
pub async fn prepare_antigravity_sandbox(
    helper: &Path,
    home: &Path,
    cwd: &Path,
    egress: &AntigravityEgress,
    bridge_socket: &Path,
    read_only: &[PathBuf],
    writable_home_paths: &[PathBuf],
) -> Result<AntigravitySandbox, String> {
    if [helper, home, cwd, bridge_socket]
        .into_iter()
        .chain(read_only.iter().map(PathBuf::as_path))
        .chain(writable_home_paths.iter().map(PathBuf::as_path))
        .any(|path| {
            path.components()
                .any(|part| part == std::path::Component::ParentDir)
        })
    {
        return Err("Antigravity confinement paths must not contain parent traversal".into());
    }
    if !helper.is_absolute() || !bridge_socket.is_absolute() {
        return Err("Antigravity helper and bridge socket must be absolute".into());
    }
    let network = network_descriptor(&egress.proxy_url, bridge_socket).await?;
    let protected = !read_only.is_empty();
    if protected && (home.starts_with(cwd) || cwd.starts_with(home)) {
        return Err("protected Antigravity profile and workspace must not overlap".into());
    }
    for path in writable_home_paths {
        if path == home
            || !path.starts_with(home)
            || read_only
                .iter()
                .any(|policy| path.starts_with(policy) || policy.starts_with(path))
        {
            return Err("Antigravity mutable state overlaps a protected policy path".into());
        }
    }
    let mut entries = vec![VfsEntry {
        path: VfsPath::Special {
            value: VfsSpecialPath::Root,
        },
        access: VfsAccessMode::Read,
    }];
    let home_access = if protected {
        VfsAccessMode::Read
    } else {
        VfsAccessMode::Write
    };
    for (path, access) in [(home, home_access), (cwd, VfsAccessMode::Write)]
        .into_iter()
        .chain(
            writable_home_paths
                .iter()
                .map(|path| (path.as_path(), VfsAccessMode::Write)),
        )
        .chain(
            read_only
                .iter()
                .map(|path| (path.as_path(), VfsAccessMode::Read)),
        )
    {
        if !path.is_absolute() {
            return Err("Antigravity filesystem policy paths must be absolute".into());
        }
        let path = AbsolutePathBuf::from_absolute_path(path)
            .map_err(|_| "invalid Antigravity filesystem policy path")?;
        entries.push(VfsEntry {
            path: VfsPath::Path { path },
            access,
        });
    }
    entries.push(VfsEntry {
        path: VfsPath::Special {
            value: VfsSpecialPath::Tmpdir,
        },
        access: VfsAccessMode::Write,
    });
    let policy = VfsPolicy::restricted(entries);
    let prepared = alcatraz::prepare_command(alcatraz::SandboxRequest {
        executable: helper,
        command: vec![],
        file_system_policy: &policy,
        network_policy: SocketPolicy::Enabled,
        sandbox_policy_cwd: cwd,
        enforce_managed_network: true,
        network: Some(&network),
        platform_permissions: None,
    })
    .map_err(|_| "failed to prepare Antigravity platform confinement")?;
    Ok(AntigravitySandbox {
        program: prepared.program,
        arg0: prepared.arg0,
        args: prepared.args,
        env: prepared.env,
    })
}

/// Frozen descriptor for an already-owned clamp proxy. It is not an additional
/// proxy, and must not reload broader ambient operator network configuration.
struct FrozenNetwork;

impl ConfigReloader for FrozenNetwork {
    fn source_label(&self) -> String {
        "Antigravity session egress".into()
    }

    fn maybe_reload(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Option<ConfigState>>> + Send + '_>,
    > {
        Box::pin(async { Ok(None) })
    }

    fn reload_now(&self) -> BoxFuture<'_, anyhow::Result<ConfigState>> {
        Box::pin(async { Err(anyhow::anyhow!("session egress policy is immutable")) })
    }
}

async fn network_descriptor(proxy: &str, bridge: &Path) -> Result<NetworkProxy, String> {
    let port = proxy
        .strip_prefix("http://127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or("Antigravity requires an owned IPv4 loopback HTTP proxy")?;
    let mut config = NetworkProxyConfig::default();
    config.network.enabled = true;
    config.network.proxy_url = proxy.into();
    config.network.enable_socks5 = false;
    config.network.enable_socks5_udp = false;
    config.network.allow_upstream_proxy = false;
    config.network.allow_local_binding = false;
    config.network.dangerously_allow_all_unix_sockets = false;
    #[cfg(target_os = "macos")]
    {
        config.network.allow_unix_sockets = vec![
            bridge
                .to_str()
                .ok_or("Antigravity bridge path must be UTF-8")?
                .into(),
        ];
    }
    #[cfg(not(target_os = "macos"))]
    let _ = bridge;
    let state = build_config_state(config, NetworkProxyConstraints::default())
        .map_err(|_| "invalid Antigravity network confinement policy")?;
    NetworkProxy::builder()
        .state(Arc::new(NetworkProxyState::with_reloader(
            state,
            Arc::new(FrozenNetwork),
        )))
        .managed_by_codex(false)
        .http_addr(([127, 0, 0, 1], port).into())
        .build()
        .await
        .map_err(|_| "failed to prepare Antigravity network confinement".into())
}

#[cfg(test)]
mod tests;
