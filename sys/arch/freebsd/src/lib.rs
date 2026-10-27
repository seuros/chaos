//! FreeBSD sandbox helper entry point.
//!
//! On FreeBSD, `alcatraz` validates whether a requested sandbox policy
//! can be enforced by `procctl` and the optional experimental `mac_gulag`
//! policy before executing the target command. Generic commands do not enter
//! Capsicum capability mode.
#[cfg(target_os = "freebsd")]
extern crate usage as usage_rs;

#[cfg(target_os = "freebsd")]
mod capsicum;
#[cfg(target_os = "freebsd")]
mod freebsd_run_main;
#[cfg(target_os = "freebsd")]
mod gulag;
/// Warden's one-command root-worker lifecycle for the private launcher transport.
#[cfg(target_os = "freebsd")]
pub mod warden;
/// Private descriptor-capability transport primitives; no public root socket.
#[cfg(target_os = "freebsd")]
pub mod warden_channel;
/// Early private bootstrap capture and helper-only descriptor inheritance.
#[cfg(target_os = "freebsd")]
pub mod warden_client;
#[cfg(target_os = "freebsd")]
mod warden_helper;
/// Warden's low-level jail ownership primitive.
#[cfg(target_os = "freebsd")]
pub mod warden_jail;
/// Explicit root launcher with a private broker capability and fixed identity.
#[cfg(target_os = "freebsd")]
pub mod warden_launch;
/// Scoped native mounts with durable ownership records and reverse cleanup.
#[cfg(target_os = "freebsd")]
pub mod warden_mount;
/// Planning-only interface for Warden's privileged jail/nullfs backend.
/// No jail, mount, or command execution is performed by this module.
#[cfg(target_os = "freebsd")]
pub mod warden_plan;
/// Identity dropping and descriptor-bound child attachment primitives.
#[cfg(target_os = "freebsd")]
pub mod warden_process;
/// Command-specific Warden RPC with explicit descriptor capabilities.
#[cfg(target_os = "freebsd")]
pub mod warden_transport;
/// Initial read-only host view, including mount boundaries and minimal devices.
#[cfg(target_os = "freebsd")]
pub mod warden_view;
#[cfg(target_os = "freebsd")]
pub use capsicum::spawn_command;

#[cfg(target_os = "freebsd")]
pub fn prepare_command(
    request: alcatraz_base::prepared_command::SandboxRequest<'_>,
) -> std::io::Result<alcatraz_base::prepared_command::PreparedCommand> {
    let sandbox_policy = request
        .file_system_policy
        .to_sandbox_policy(request.network_policy, request.sandbox_policy_cwd)
        .map_err(|source| std::io::Error::new(std::io::ErrorKind::InvalidInput, source))?;
    capsicum::prepare_command_from_policies(
        request.executable,
        request.command,
        &sandbox_policy,
        request.file_system_policy,
        request.network_policy,
        request.sandbox_policy_cwd,
        request.enforce_managed_network,
    )
}

#[cfg(target_os = "freebsd")]
pub fn run_main() -> ! {
    freebsd_run_main::run_main();
}

#[cfg(not(target_os = "freebsd"))]
pub fn run_main() -> ! {
    panic!("alcatraz is only supported on FreeBSD");
}

/// Register the platform credential store for FreeBSD.
///
/// Uses D-Bus Secret Service (via libdbus).
#[cfg(target_os = "freebsd")]
pub fn register_keyring_store() {
    use keyring_core::set_default_store;

    if let Ok(store) = dbus_secret_service_keyring_store::Store::new() {
        set_default_store(store);
    }
}

#[cfg(not(target_os = "freebsd"))]
pub fn register_keyring_store() {}
