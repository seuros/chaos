#[cfg(target_os = "macos")]
mod pid_tracker;
#[cfg(target_os = "macos")]
mod seatbelt;

#[cfg(target_os = "macos")]
use alcatraz_macos::seatbelt::create_seatbelt_command_args;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::Stdio;

use chaos_getopt::CliConfigOverrides;
use chaos_ipc::config_types::SandboxMode;
#[cfg(target_os = "macos")]
use chaos_ipc::permissions::NetworkSandboxPolicy;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::NetworkProxyAuditMetadata;
use chaos_kern::exec_env::create_env;
use chaos_kern::landlock::spawn_command_under_linux_sandbox;
#[cfg(target_os = "macos")]
use chaos_kern::spawn::CODEX_SANDBOX_ENV_VAR;
#[cfg(target_os = "macos")]
use chaos_kern::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use chaos_kern::spawn::StdioPolicy;
#[cfg(target_os = "macos")]
use tokio::process::Child;
#[cfg(target_os = "macos")]
use tokio::process::Command;

use crate::LandlockCommand;
use crate::SeatbeltCommand;
use crate::exit_status::handle_exit_status;

#[cfg(target_os = "macos")]
use seatbelt::DenialLogger;

#[cfg(target_os = "macos")]
pub async fn run_command_under_seatbelt(
    command: SeatbeltCommand,
    alcatraz_macos_exe: Option<PathBuf>,
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
        alcatraz_macos_exe,
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
    _alcatraz_macos_exe: Option<PathBuf>,
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
        None,
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
        None,
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
    alcatraz_macos_exe: Option<PathBuf>,
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
            alcatraz_macos_exe,
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
            #[expect(clippy::expect_used)]
            let alcatraz_macos_exe = config
                .alcatraz_macos_exe
                .expect("alcatraz-macos executable not found");
            spawn_command_under_macos_seatbelt(
                alcatraz_macos_exe,
                command,
                cwd,
                config.permissions.sandbox_policy.get(),
                sandbox_policy_cwd.as_path(),
                stdio_policy,
                managed_network_requirements_enabled,
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

#[cfg(target_os = "macos")]
async fn spawn_command_under_macos_seatbelt(
    alcatraz_macos_exe: PathBuf,
    command: Vec<String>,
    command_cwd: PathBuf,
    sandbox_policy: &chaos_ipc::protocol::SandboxPolicy,
    sandbox_cwd: &std::path::Path,
    stdio_policy: StdioPolicy,
    enforce_managed_network: bool,
    network: Option<&chaos_pf::NetworkProxy>,
    mut env: std::collections::HashMap<String, String>,
) -> std::io::Result<Child> {
    let args = create_seatbelt_command_args(
        command,
        sandbox_policy,
        sandbox_cwd,
        enforce_managed_network,
        network,
    );

    if let Some(network) = network {
        network.apply_to_env(&mut env);
    }
    env.insert(CODEX_SANDBOX_ENV_VAR.to_string(), "seatbelt".to_string());
    if !NetworkSandboxPolicy::from(sandbox_policy).is_enabled() {
        env.insert(
            CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR.to_string(),
            "1".to_string(),
        );
    }

    let mut cmd = Command::new(&alcatraz_macos_exe);
    cmd.arg0("alcatraz-macos");
    cmd.args(args);
    cmd.current_dir(command_cwd);
    cmd.env_clear();
    cmd.envs(env);

    match stdio_policy {
        StdioPolicy::RedirectForShellTool => {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        StdioPolicy::Inherit => {
            cmd.stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        }
    }

    cmd.kill_on_drop(true).spawn()
}
