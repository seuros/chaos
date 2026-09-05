//! FreeBSD sandbox helper entry point.
//!
//! On FreeBSD, `alcatraz` validates whether a requested sandbox policy
//! can be enforced safely by the current Capsicum-backed helper and then execs
//! the target command.
#[cfg(target_os = "freebsd")]
mod capsicum;
#[cfg(target_os = "freebsd")]
mod freebsd_run_main;
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
