//! macOS sandbox helper entry point.
//!
//! On macOS, `alcatraz` is a thin helper that re-execs the trusted
//! `/usr/bin/sandbox-exec` binary with the Seatbelt arguments supplied by the
//! caller.

#[cfg(target_os = "macos")]
pub mod seatbelt;
#[cfg(target_os = "macos")]
pub mod seatbelt_permissions;

pub mod protocol {
    pub use chaos_ipc::permissions::VfsSpecialPath;
    pub use chaos_ipc::protocol::*;
}

#[cfg(target_os = "macos")]
mod macos_run_main;

#[cfg(target_os = "macos")]
pub fn prepare_command(
    request: alcatraz_base::prepared_command::SandboxRequest<'_>,
) -> std::io::Result<alcatraz_base::prepared_command::PreparedCommand> {
    let extensions = request
        .platform_permissions
        .and_then(|permissions| permissions.macos.as_ref());
    let args = seatbelt::create_seatbelt_command_args_for_policies_with_extensions(
        request.command,
        request.file_system_policy,
        request.network_policy,
        request.sandbox_policy_cwd,
        request.enforce_managed_network,
        request.network,
        extensions,
    );

    Ok(alcatraz_base::prepared_command::PreparedCommand {
        program: request.executable.to_path_buf(),
        args,
        env: [("CHAOS_SANDBOX".to_string(), "seatbelt".to_string())]
            .into_iter()
            .collect(),
        arg0: Some("alcatraz".to_string()),
    })
}

#[cfg(target_os = "macos")]
pub fn run_main() -> ! {
    macos_run_main::run_main();
}

#[cfg(not(target_os = "macos"))]
pub fn run_main() -> ! {
    panic!("alcatraz is only supported on macOS");
}
