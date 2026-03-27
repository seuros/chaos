#[cfg(target_os = "macos")]
mod pid_tracker;
#[cfg(target_os = "macos")]
mod seatbelt;

use std::path::PathBuf;

use chaos_kern::config::Config;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::NetworkProxyAuditMetadata;
use chaos_kern::exec_env::create_env;
use chaos_kern::landlock::spawn_command_under_linux_sandbox;
#[cfg(target_os = "macos")]
use chaos_kern::seatbelt::spawn_command_under_seatbelt;
use chaos_kern::spawn::StdioPolicy;
use chaos_ipc::config_types::SandboxMode;
use chaos_getopt::CliConfigOverrides;

use crate::LandlockCommand;
use crate::SeatbeltCommand;
use crate::exit_status::handle_exit_status;

#[cfg(target_os = "macos")]
use seatbelt::DenialLogger;

#[cfg(target_os = "macos")]
pub async fn run_command_under_seatbelt(
    command: SeatbeltCommand,
    alcatraz_linux_exe: Option<PathBuf>,
    alcatraz_freebsd_exe: Option<PathBuf>,
) -> anyhow::Result<()> {
    let SeatbeltCommand {
        full_auto,
        log_denials,
        config_overrides,
        command,
    } = command;
    run_command_under_sandbox(
        full_auto,
        command,
        config_overrides,
        alcatraz_linux_exe,
        alcatraz_freebsd_exe,
        SandboxType::Seatbelt,
        log_denials,
    )
    .await
}

#[cfg(not(target_os = "macos"))]
pub async fn run_command_under_seatbelt(
    _command: SeatbeltCommand,
    _alcatraz_linux_exe: Option<PathBuf>,
    _alcatraz_freebsd_exe: Option<PathBuf>,
) -> anyhow::Result<()> {
    anyhow::bail!("Seatbelt sandbox is only available on macOS");
}

pub async fn run_command_under_landlock(
    command: LandlockCommand,
    alcatraz_linux_exe: Option<PathBuf>,
    alcatraz_freebsd_exe: Option<PathBuf>,
) -> anyhow::Result<()> {
    let LandlockCommand {
        full_auto,
        config_overrides,
        command,
    } = command;
    run_command_under_sandbox(
        full_auto,
        command,
        config_overrides,
        alcatraz_linux_exe,
        alcatraz_freebsd_exe,
        SandboxType::Landlock,
        /*log_denials*/ false,
    )
    .await
}

#[cfg(target_os = "freebsd")]
pub async fn run_command_under_capsicum(
    command: LandlockCommand,
    alcatraz_linux_exe: Option<PathBuf>,
    alcatraz_freebsd_exe: Option<PathBuf>,
) -> anyhow::Result<()> {
    let LandlockCommand {
        full_auto,
        config_overrides,
        command,
    } = command;
    run_command_under_sandbox(
        full_auto,
        command,
        config_overrides,
        alcatraz_linux_exe,
        alcatraz_freebsd_exe,
        SandboxType::Capsicum,
        /*log_denials*/ false,
    )
    .await
}

enum SandboxType {
    #[cfg(target_os = "macos")]
    Seatbelt,
    Landlock,
    #[cfg(target_os = "freebsd")]
    Capsicum,
}

async fn run_command_under_sandbox(
    full_auto: bool,
    command: Vec<String>,
    config_overrides: CliConfigOverrides,
    alcatraz_linux_exe: Option<PathBuf>,
    alcatraz_freebsd_exe: Option<PathBuf>,
    sandbox_type: SandboxType,
    log_denials: bool,
) -> anyhow::Result<()> {
    let sandbox_mode = create_sandbox_mode(full_auto);
    let config = Config::load_with_cli_overrides_and_harness_overrides(
        config_overrides
            .parse_overrides()
            .map_err(anyhow::Error::msg)?,
        ConfigOverrides {
            sandbox_mode: Some(sandbox_mode),
            alcatraz_linux_exe,
            alcatraz_freebsd_exe,
            ..Default::default()
        },
    )
    .await?;

    // In practice, this should be `std::env::current_dir()` because this CLI
    // does not support `--cwd`, but let's use the config value for consistency.
    let cwd = config.cwd.clone();
    // For now, we always use the same cwd for both the command and the
    // sandbox policy. In the future, we could add a CLI option to set them
    // separately.
    let sandbox_policy_cwd = cwd.clone();

    let stdio_policy = StdioPolicy::Inherit;
    let env = create_env(
        &config.permissions.shell_environment_policy,
        /*process_id*/ None,
    );

    #[cfg(target_os = "macos")]
    let mut denial_logger = log_denials.then(DenialLogger::new).flatten();
    #[cfg(not(target_os = "macos"))]
    let _ = log_denials;

    let managed_network_requirements_enabled = config.managed_network_requirements_enabled();

    // This proxy should only live for the lifetime of the child process.
    let network_proxy = match config.permissions.network.as_ref() {
        Some(spec) => Some(
            spec.start_proxy(
                config.permissions.sandbox_policy.get(),
                /*policy_decider*/ None,
                /*blocked_request_observer*/ None,
                managed_network_requirements_enabled,
                NetworkProxyAuditMetadata::default(),
            )
            .await
            .map_err(|err| anyhow::anyhow!("failed to start managed network proxy: {err}"))?,
        ),
        None => None,
    };
    let network = network_proxy
        .as_ref()
        .map(chaos_kern::config::StartedNetworkProxy::proxy);

    let mut child = match sandbox_type {
        #[cfg(target_os = "macos")]
        SandboxType::Seatbelt => {
            spawn_command_under_seatbelt(
                command,
                cwd,
                config.permissions.sandbox_policy.get(),
                sandbox_policy_cwd.as_path(),
                stdio_policy,
                network.as_ref(),
                env,
            )
            .await?
        }
        SandboxType::Landlock => {
            #[expect(clippy::expect_used)]
            let alcatraz_linux_exe = config
                .alcatraz_linux_exe
                .expect("alcatraz-linux executable not found");
            spawn_command_under_linux_sandbox(
                alcatraz_linux_exe,
                command,
                cwd,
                config.permissions.sandbox_policy.get(),
                sandbox_policy_cwd.as_path(),
                stdio_policy,
                network.as_ref(),
                env,
            )
            .await?
        }
        #[cfg(target_os = "freebsd")]
        SandboxType::Capsicum => {
            // Always dispatch to the helper — it applies procctl hardening
            // and warns about unenforced dimensions internally.
            #[expect(clippy::expect_used)]
            let alcatraz_freebsd_exe = config
                .alcatraz_freebsd_exe
                .expect("alcatraz-freebsd executable not found");
            chaos_kern::capsicum::spawn_command_under_freebsd_sandbox(
                alcatraz_freebsd_exe,
                command,
                cwd,
                config.permissions.sandbox_policy.get(),
                sandbox_policy_cwd.as_path(),
                stdio_policy,
                network.as_ref(),
                env,
            )
            .await?
        }
    };

    #[cfg(target_os = "macos")]
    if let Some(denial_logger) = &mut denial_logger {
        denial_logger.on_child_spawn(&child);
    }

    let status = child.wait().await?;

    #[cfg(target_os = "macos")]
    if let Some(denial_logger) = denial_logger {
        let denials = denial_logger.finish().await;
        eprintln!("\n=== Sandbox denials ===");
        if denials.is_empty() {
            eprintln!("None found.");
        } else {
            for seatbelt::SandboxDenial { name, capability } in denials {
                eprintln!("({name}) {capability}");
            }
        }
    }

    handle_exit_status(status);
}

pub fn create_sandbox_mode(full_auto: bool) -> SandboxMode {
    if full_auto {
        SandboxMode::WorkspaceWrite
    } else {
        SandboxMode::ReadOnly
    }
}
