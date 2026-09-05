#[cfg(target_os = "macos")]
mod pid_tracker;
#[cfg(target_os = "macos")]
mod seatbelt;

use chaos_ipc::config_types::SandboxMode;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::NetworkProxyAuditMetadata;
use chaos_kern::exec_env::create_env;
use chaos_kern::spawn::CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR;
#[cfg(target_os = "macos")]
use seatbelt::DenialLogger;
use tokio::process::Command;

use crate::SandboxCommand;
use crate::exit_status::handle_exit_status;

pub async fn run_command_under_sandbox(
    sandbox_command: SandboxCommand,
    alcatraz_exe: std::path::PathBuf,
) -> anyhow::Result<()> {
    let SandboxCommand {
        full_auto,
        #[cfg(target_os = "macos")]
        log_denials,
        config_overrides,
        command,
    } = sandbox_command;

    let sandbox_mode = create_sandbox_mode(full_auto);
    let config = Config::load_with_cli_overrides_and_harness_overrides(
        config_overrides
            .parse_overrides()
            .map_err(anyhow::Error::msg)?,
        ConfigOverrides {
            sandbox_mode: Some(sandbox_mode),
            alcatraz_exe: Some(alcatraz_exe),
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

    let mut env = create_env(
        &config.permissions.shell_environment_policy,
        /*process_id*/ None,
    );

    #[cfg(target_os = "macos")]
    let mut denial_logger = log_denials.then(DenialLogger::new).flatten();

    let managed_network_requirements_enabled = config.managed_network_requirements_enabled();

    // This proxy should only live for the lifetime of the child process.
    let network_proxy = match config.permissions.network.as_ref() {
        Some(spec) => Some(
            spec.start_proxy(
                &config.permissions.vfs_policy,
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

    let alcatraz_exe = config.alcatraz_exe.as_path();
    let prepared = alcatraz::prepare_command(alcatraz::SandboxRequest {
        executable: alcatraz_exe,
        command,
        file_system_policy: &config.permissions.vfs_policy,
        network_policy: config.permissions.socket_policy,
        sandbox_policy_cwd: sandbox_policy_cwd.as_path(),
        enforce_managed_network: managed_network_requirements_enabled,
        network: network.as_ref(),
        platform_permissions: None,
    })?;

    if let Some(network) = network.as_ref() {
        network.apply_to_env(&mut env);
    }
    env.extend(prepared.env);
    if !config.permissions.socket_policy.is_enabled() {
        env.insert(
            CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR.to_string(),
            "1".to_string(),
        );
    }

    let mut command = Command::new(prepared.program);
    if let Some(arg0) = prepared.arg0 {
        command.arg0(arg0);
    };
    command.args(prepared.args);
    command.current_dir(cwd);
    command.env_clear();
    command.envs(env);
    let mut child = command.kill_on_drop(true).spawn()?;

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
