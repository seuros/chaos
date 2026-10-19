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

/// Starts the Antigravity egress proxy and returns the environment the CLI
/// needs to route through it, along with the handle keeping it alive.
/// Generation requests receive the canonical system prompt at this boundary;
/// callers must refresh it on the returned proxy before subsequent turns.
pub async fn start_antigravity_egress(
    sink: Arc<dyn WiretapSink>,
    ca_bundle_path: PathBuf,
    gateway: Option<chaos_client::Egress>,
    system_prompt: String,
) -> Result<(EgressProxy, AntigravityEgress), String> {
    let proxy = EgressProxy::start(
        EgressPolicy::antigravity()
            .with_gateway(gateway)
            .with_antigravity_system_prompt(system_prompt),
        sink,
        Some(ca_bundle_path),
    )
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
/// The platform backend receives the exact proxy and bridge endpoints rather
/// than inferring ambient networking or receiving another platform's arguments.
pub async fn antigravity_sandbox(
    helper: &Path,
    home: Option<&Path>,
    cwd: &Path,
    egress: &AntigravityEgress,
    bridge_socket: &Path,
) -> Result<AntigravitySandbox, String> {
    let home = home.ok_or("Antigravity sandbox requires a dedicated home")?;
    chaos_clamp::prepare_antigravity_sandbox(helper, home, cwd, egress, bridge_socket, &[], &[])
        .await
}

#[cfg(test)]
mod tests;
