// mimalloc reclaims per-thread arenas faster than glibc's default malloc, which
// keeps RSS flatter during long model streams where Chaos allocates and frees
// millions of small buffers per conversation turn.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use chaos_argv::Arg0DispatchPaths;
use chaos_argv::arg0_dispatch_or_else;
use chaos_boot::accounts::read_api_key_from_stdin;
use chaos_boot::accounts::run_accounts_status;
use chaos_boot::accounts::run_accounts_usage;
use chaos_boot::accounts::run_connect_with_api_key;
use chaos_boot::accounts::run_connect_with_chatgpt_account;
use chaos_boot::accounts::run_connect_with_device_code;
use chaos_boot::accounts::run_disconnect;
#[cfg(feature = "tui")]
use chaos_console::AppExitInfo;
#[cfg(feature = "tui")]
use chaos_console::Cli as TuiCli;
#[cfg(feature = "tui")]
use chaos_console::ExitReason;
use chaos_fork::Cli as ExecCli;
use chaos_fork::Command as ExecCommand;
use chaos_fork::ReviewArgs;
use chaos_getopt::CliConfigOverrides;
#[cfg(feature = "tui")]
use chaos_ipc::product::OS_NAME;
use chaos_selinux::ExecPolicyCheckCommand;
#[cfg(feature = "tui")]
use owo_colors::OwoColorize;
#[cfg(feature = "tui")]
use std::io::IsTerminal;
#[cfg(feature = "tui")]
use supports_color::Stream;

mod config_cmd;
mod debug_logging;
mod mcp_cmd;
mod models_cmd;
mod reflex_cmd;

use crate::mcp_cmd::McpCli;
use crate::models_cmd::ModelsCli;

#[cfg(feature = "tui")]
use chaos_kern::terminal::TerminalName;

/// Chaos
///
#[cfg_attr(
    feature = "tui",
    doc = "If no subcommand is specified, options will be forwarded to the interactive CLI."
)]
#[cfg_attr(
    not(feature = "tui"),
    doc = "Built without the TUI. Use serve, mcp serve, taskd, or exec for headless operation."
)]
#[derive(Debug, usage::Cli)]
#[cfg_attr(
    feature = "tui",
    usage(usage = "chaos [OPTIONS] [PROMPT]\n       chaos [OPTIONS] <COMMAND> [ARGS]")
)]
#[cfg_attr(
    not(feature = "tui"),
    usage(usage = "chaos [OPTIONS] <COMMAND> [ARGS]")
)]
#[usage(
    author = env!("CARGO_PKG_AUTHORS"),
    version = concat!(env!("CARGO_PKG_VERSION"), ".", env!("CHAOS_BUILD_TS")),
    // The executable is sometimes invoked via a platform‑specific name like
    // `chaos-x86_64-unknown-linux-musl`, but the help output should always use
    // the generic `chaos` command name that users run.
    bin = "chaos",
    completion,
    unknown_flags = "error",
    args_override_self = false
)]
struct MultitoolCli {
    /// Enable debug logging to ~/.chaos/debug.log.
    #[usage(short = 'd', long = "debug", global)]
    debug: bool,

    /// Override the model provider (e.g. openai, anthropic, charm). Equivalent to `-c model_provider=<name>`.
    #[usage(long = "provider", value_name = "PROVIDER", global)]
    provider: Option<String>,

    #[usage(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[cfg(feature = "tui")]
    #[usage(flatten)]
    interactive: TuiCli,

    // Models also uses the root profile flag; do not require the console to parse it.
    /// Configuration profile from config.toml to specify default options.
    #[cfg(not(feature = "tui"))]
    #[usage(long = "profile", short = 'p')]
    config_profile: Option<String>,

    #[usage(subcommand)]
    subcommand: Option<Subcommand>,
}

impl MultitoolCli {
    fn config_profile(&self) -> Option<&str> {
        #[cfg(feature = "tui")]
        {
            self.interactive.config_profile.as_deref()
        }
        #[cfg(not(feature = "tui"))]
        {
            self.config_profile.as_deref()
        }
    }

    fn normalize(&mut self) {
        if let Some(Subcommand::Exec(exec)) = &mut self.subcommand {
            exec.normalize();
        }
    }
}

#[derive(Debug, usage::Subcommands)]
enum Subcommand {
    /// Manage database user settings and bootstrap recovery.
    Config(config_cmd::ConfigCommand),
    /// Inspect or revoke installation-local remembered approvals.
    Approvals(config_cmd::ApprovalsCommand),
    /// Diagnose configured reflex backends without sending session contents.
    Reflex(reflex_cmd::ReflexCommand),
    /// Run Chaos non-interactively.
    #[usage(alias = "e")]
    Exec(ExecCli),

    /// Run a code review non-interactively.
    Review(ReviewArgs),

    /// Manage provider accounts and connections.
    #[usage(alias = "login")]
    Accounts(AccountsCommand),

    /// Disconnect stored provider accounts.
    Logout(LogoutCommand),

    /// Manage external MCP servers for Chaos.
    Mcp(McpCli),

    /// Generate shell completion scripts.
    Completion(CompletionCommand),

    /// Run a command inside the platform sandbox (landlock on Linux, seatbelt on macOS).
    Sandbox(chaos_boot::SandboxCommand),

    /// Execpolicy tooling.
    #[usage(hide)]
    Execpolicy(ExecpolicyCommand),

    /// Resume a previous interactive session (picker by default; use --last to continue the most recent).
    #[cfg(feature = "tui")]
    Resume(ResumeCommand),

    /// Fork a previous interactive session (picker by default; use --last to fork the most recent).
    #[cfg(feature = "tui")]
    Fork(ForkCommand),

    /// Run Chaos as an HTTP trigger server.
    Serve(chaos_httpd::ServeCli),
    /// Recover durable background work through the kernel.
    Taskd(chaos_taskd::TaskdCli),

    /// List available models for the active provider.
    Models(ModelsCli),

    /// Hidden MCP bridge used by clamp subprocesses.
    #[usage(hide, name = "clamp-session-bridge")]
    ClampSessionBridge,
}

#[derive(Debug, usage::Args)]
struct CompletionCommand {
    /// Shell to generate completions for
    #[usage(value_enum, default = "bash")]
    shell: CompletionShell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, usage::ValueEnum)]
enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    #[usage(name = "nushell", alias = "nu")]
    Nushell,
    #[usage(name = "powershell", alias = "pwsh")]
    PowerShell,
    Zsh,
}

impl From<CompletionShell> for usage::complete::Shell {
    fn from(shell: CompletionShell) -> Self {
        match shell {
            CompletionShell::Bash => Self::Bash,
            CompletionShell::Elvish => Self::Elvish,
            CompletionShell::Fish => Self::Fish,
            CompletionShell::Nushell => Self::Nu,
            CompletionShell::PowerShell => Self::PowerShell,
            CompletionShell::Zsh => Self::Zsh,
        }
    }
}

#[cfg(feature = "tui")]
#[derive(Debug, usage::Args)]
struct ResumeCommand {
    /// Conversation/session id (UUID) or thread name. UUIDs take precedence if it parses.
    /// If omitted, use --last to pick the most recent recorded session.
    #[usage(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Continue the most recent session without showing the picker.
    #[usage(long = "last")]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[usage(long = "all")]
    all: bool,

    #[usage(flatten)]
    config_overrides: TuiCli,
}

#[cfg(feature = "tui")]
#[derive(Debug, usage::Args)]
struct ForkCommand {
    /// Conversation/session id (UUID). When provided, forks this session.
    /// If omitted, use --last to pick the most recent recorded session.
    #[usage(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Fork the most recent session without showing the picker.
    #[usage(long = "last", conflicts = "session_id")]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[usage(long = "all")]
    all: bool,

    #[usage(flatten)]
    config_overrides: TuiCli,
}

// SandboxCommand is defined in chaos_boot::SandboxCommand — platform-agnostic,
// auto-dispatches to seatbelt (macOS) or landlock (Linux) at runtime.

#[derive(Debug, usage::Args)]
struct ExecpolicyCommand {
    #[usage(subcommand)]
    sub: ExecpolicySubcommand,
}

#[derive(Debug, usage::Subcommands)]
enum ExecpolicySubcommand {
    /// Check execpolicy files against a command.
    #[usage(name = "check")]
    Check(ExecPolicyCheckCommand),
}

#[derive(Debug, usage::Args)]
struct AccountsCommand {
    #[usage(skip)]
    config_overrides: CliConfigOverrides,

    /// Read the API key from stdin (e.g. `printenv OPENAI_API_KEY | chaos accounts --with-api-key`).
    #[usage(long = "with-api-key")]
    with_api_key: bool,

    /// Connect the selected provider with a subscription account using device authorization.
    #[usage(long = "device-auth")]
    use_device_code: bool,

    /// EXPERIMENTAL: Use custom OAuth issuer base URL (advanced)
    /// Override the OAuth issuer base URL (advanced)
    #[usage(long = "experimental_issuer", value_name = "URL", hide)]
    issuer_base_url: Option<String>,

    /// EXPERIMENTAL: Use custom OAuth client ID (advanced)
    #[usage(long = "experimental_client-id", value_name = "CLIENT_ID", hide)]
    client_id: Option<String>,

    #[usage(subcommand)]
    action: Option<AccountsSubcommand>,
}

#[derive(Debug, usage::Subcommands)]
enum AccountsSubcommand {
    /// Show provider account status.
    Status,

    /// Print subscription usage for the selected provider.
    Usage {
        /// Emit the stable machine-readable JSON response.
        #[usage(long)]
        json: bool,
    },

    /// Disconnect stored provider accounts.
    Disconnect {
        /// Disconnect every stored provider account instead of only the active provider.
        #[usage(long = "all")]
        all: bool,
    },
}

#[derive(Debug, usage::Args)]
struct LogoutCommand {
    #[usage(skip)]
    config_overrides: CliConfigOverrides,
}

#[cfg(feature = "tui")]
fn format_exit_messages(exit_info: AppExitInfo, color_enabled: bool) -> Vec<String> {
    let AppExitInfo {
        token_usage,
        process_id: conversation_id,
        process_name,
        ..
    } = exit_info;

    if token_usage.is_zero() {
        return Vec::new();
    }

    let mut lines = vec![format!(
        "{}",
        chaos_ipc::protocol::FinalOutput::from(token_usage)
    )];

    let resume_commands =
        chaos_kern::util::resume_commands(process_name.as_deref(), conversation_id);
    let has_name_and_id = resume_commands.len() == 2;
    for (index, resume_cmd) in resume_commands.into_iter().enumerate() {
        let command = if color_enabled {
            resume_cmd.cyan().to_string()
        } else {
            resume_cmd
        };
        let prefix = if index == 0 && has_name_and_id {
            "To continue this session by name, run "
        } else if index == 0 {
            "To continue this session, run "
        } else {
            "Or by session ID, run "
        };
        lines.push(format!("{prefix}{command}"));
    }

    lines
}

/// Handle the app exit and print the results. Optionally run the update action.
#[cfg(feature = "tui")]
fn handle_app_exit(exit_info: AppExitInfo) -> anyhow::Result<()> {
    match exit_info.exit_reason {
        ExitReason::Fatal(message) => {
            eprintln!("ERROR: {message}");
            std::process::exit(1);
        }
        ExitReason::UserRequested => { /* normal exit */ }
    }

    let color_enabled = supports_color::on(Stream::Stdout).is_some();
    for line in format_exit_messages(exit_info, color_enabled) {
        println!("{line}");
    }
    Ok(())
}

fn run_execpolicycheck(cmd: ExecPolicyCheckCommand) -> anyhow::Result<()> {
    cmd.run()
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|arg0_paths: Arg0DispatchPaths| async move {
        // Sandbox helpers dispatch above and never reach this point, so any
        // init that spawns background threads (keyring D-Bus, TLS providers)
        // runs only in the regular chaos process — clear of the seccomp filter.
        let _ = rama::tls::rustls::dep::rustls::crypto::ring::default_provider().install_default();
        alcatraz::register_keyring_store();

        cli_main(arg0_paths).await?;
        Ok(())
    })
}

/// Prepend `$root` (a `CliConfigOverrides`) into `$target.config_overrides`,
/// consuming a clone so the caller can reuse `root_config_overrides` in
/// subsequent arms.
macro_rules! prepend_root_flags {
    ($target:expr, $root:expr) => {
        prepend_config_flags(&mut $target.config_overrides, $root.clone())
    };
}

async fn cli_main(arg0_paths: Arg0DispatchPaths) -> anyhow::Result<()> {
    let mut cli = MultitoolCli::parse();
    cli.normalize();
    let models_profile = cli.config_profile().map(str::to_owned);

    let MultitoolCli {
        debug,
        provider,
        config_overrides: mut root_config_overrides,
        #[cfg(feature = "tui")]
        interactive,
        subcommand,
        ..
    } = cli;

    // If --debug was passed, prepare the shared debug.log path before anything
    // else. The concrete runtime attaches the actual tracing layer so it can
    // compose with its existing subscriber stack.
    if debug {
        debug_logging::prepare_debug_logging()?;
    }

    // Fold --provider into config overrides so it flows to all subcommands.
    let models_provider = provider.clone();
    if let Some(p) = provider {
        root_config_overrides
            .raw_overrides
            .push(format!("model_provider={p}"));
    }

    match subcommand {
        Some(Subcommand::Config(command)) => config_cmd::run(command).await?,
        Some(Subcommand::Approvals(command)) => config_cmd::approvals(command).await?,
        Some(Subcommand::Reflex(command)) => {
            reflex_cmd::run(command, root_config_overrides, models_profile).await?
        }
        #[cfg(feature = "tui")]
        command @ (None | Some(Subcommand::Resume(_)) | Some(Subcommand::Fork(_))) => {
            let interactive = finalize_interactive(interactive, root_config_overrides, command);
            let exit_info = run_interactive_tui(interactive, arg0_paths.clone()).await?;
            handle_app_exit(exit_info)?;
        }
        #[cfg(not(feature = "tui"))]
        None => anyhow::bail!(
            "this build has no TUI; use `chaos serve`, `chaos mcp serve`, `chaos taskd`, \
             or `chaos exec`. Rebuild with `--features tui` for interactive mode."
        ),
        Some(Subcommand::Exec(mut exec_cli)) => {
            prepend_root_flags!(exec_cli, root_config_overrides);
            chaos_fork::run_main(exec_cli, arg0_paths.clone()).await?;
        }
        Some(Subcommand::Review(review_args)) => {
            let mut exec_cli = ExecCli {
                command: Some(ExecCommand::Review(review_args)),
                ..Default::default()
            };
            prepend_root_flags!(exec_cli, root_config_overrides);
            chaos_fork::run_main(exec_cli, arg0_paths.clone()).await?;
        }
        Some(Subcommand::Mcp(mut mcp_cli)) => {
            if matches!(mcp_cli.subcommand, crate::mcp_cmd::McpSubcommand::Serve) {
                chaos_mcpd::run_main(arg0_paths.clone(), root_config_overrides).await?;
            } else {
                prepend_root_flags!(mcp_cli, root_config_overrides);
                mcp_cli.run().await?;
            }
        }
        Some(Subcommand::Serve(serve_cli)) => {
            chaos_httpd::run_main(arg0_paths.clone(), root_config_overrides, serve_cli).await?;
        }
        Some(Subcommand::Taskd(cli)) => {
            chaos_taskd::run_main(arg0_paths.clone(), root_config_overrides, cli).await?;
        }
        Some(Subcommand::Accounts(mut accounts_cli)) => {
            prepend_root_flags!(accounts_cli, root_config_overrides);
            match accounts_cli.action {
                Some(AccountsSubcommand::Status) => {
                    run_accounts_status(accounts_cli.config_overrides).await;
                }
                Some(AccountsSubcommand::Usage { json }) => {
                    run_accounts_usage(accounts_cli.config_overrides, json).await;
                }
                Some(AccountsSubcommand::Disconnect { all }) => {
                    run_disconnect(accounts_cli.config_overrides, all).await;
                }
                None => {
                    if accounts_cli.use_device_code {
                        run_connect_with_device_code(
                            accounts_cli.config_overrides,
                            accounts_cli.issuer_base_url,
                            accounts_cli.client_id,
                        )
                        .await;
                    } else if accounts_cli.with_api_key {
                        let api_key = read_api_key_from_stdin();
                        run_connect_with_api_key(accounts_cli.config_overrides, api_key).await;
                    } else {
                        run_connect_with_chatgpt_account(accounts_cli.config_overrides).await;
                    }
                }
            }
        }
        Some(Subcommand::Logout(mut logout_cli)) => {
            prepend_root_flags!(logout_cli, root_config_overrides);
            run_disconnect(logout_cli.config_overrides, /*all*/ true).await;
        }
        Some(Subcommand::Completion(completion_cli)) => {
            print_completion(completion_cli);
        }
        Some(Subcommand::Sandbox(mut sandbox_cmd)) => {
            prepend_root_flags!(sandbox_cmd, root_config_overrides);
            chaos_boot::debug_sandbox::run_command_under_sandbox(
                sandbox_cmd,
                arg0_paths.alcatraz_exe.clone(),
            )
            .await?;
        }
        Some(Subcommand::Execpolicy(ExecpolicyCommand { sub })) => match sub {
            ExecpolicySubcommand::Check(cmd) => run_execpolicycheck(cmd)?,
        },
        Some(Subcommand::Models(cli)) => {
            models_cmd::run(cli, models_profile, models_provider).await?;
        }
        Some(Subcommand::ClampSessionBridge) => {
            chaos_mcpd::run_clamp_session_bridge_main().await?;
        }
    }

    Ok(())
}

/// Prepend root-level overrides so they have lower precedence than
/// CLI-specific ones specified after the subcommand (if any).
fn prepend_config_flags(
    subcommand_config_overrides: &mut CliConfigOverrides,
    cli_config_overrides: CliConfigOverrides,
) {
    subcommand_config_overrides
        .raw_overrides
        .splice(0..0, cli_config_overrides.raw_overrides);
}

#[cfg(feature = "tui")]
async fn run_interactive_tui(
    interactive: TuiCli,
    arg0_paths: Arg0DispatchPaths,
) -> std::io::Result<AppExitInfo> {
    let terminal_info = chaos_kern::terminal::terminal_info();
    if terminal_info.name == TerminalName::Dumb {
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            return Ok(AppExitInfo::fatal(
                "TERM is set to \"dumb\". Refusing to start the interactive TUI because no terminal is available for a confirmation prompt (stdin/stderr is not a TTY). Run in a supported terminal or unset TERM.",
            ));
        }

        eprintln!(
            "WARNING: TERM is set to \"dumb\". {OS_NAME}'s interactive TUI may not work in this terminal."
        );
        if !confirm("Continue anyway? [y/N]: ")? {
            return Ok(AppExitInfo::fatal(
                "Refusing to start the interactive TUI because TERM is set to \"dumb\". Run in a supported terminal or unset TERM.",
            ));
        }
    }

    chaos_console::run_main(
        interactive,
        arg0_paths,
        chaos_kern::config_loader::LoaderOverrides::default(),
    )
    .await
}

#[cfg(feature = "tui")]
fn confirm(prompt: &str) -> std::io::Result<bool> {
    eprintln!("{prompt}");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Finalize fresh, resumed, and forked interactive sessions through one merge path.
#[cfg(feature = "tui")]
fn finalize_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    command: Option<Subcommand>,
) -> TuiCli {
    let subcommand_cli = match command {
        Some(Subcommand::Resume(cmd)) => {
            interactive.resume_picker = cmd.session_id.is_none() && !cmd.last;
            interactive.resume_last = cmd.last;
            interactive.resume_session_id = cmd.session_id;
            interactive.resume_show_all = cmd.all;
            Some(cmd.config_overrides)
        }
        Some(Subcommand::Fork(cmd)) => {
            interactive.fork_picker = cmd.session_id.is_none() && !cmd.last;
            interactive.fork_last = cmd.last;
            interactive.fork_session_id = cmd.session_id;
            interactive.fork_show_all = cmd.all;
            Some(cmd.config_overrides)
        }
        None => None,
        _ => unreachable!("expected an interactive command"),
    };

    if let Some(subcommand_cli) = subcommand_cli {
        merge_interactive_cli_flags(&mut interactive, subcommand_cli);
    }
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);
    if let Some(prompt) = interactive.prompt.take() {
        // Normalize once, after selecting the highest-precedence prompt.
        interactive.prompt = Some(prompt.replace("\r\n", "\n").replace('\r', "\n"));
    }

    interactive
}

/// Merge flags provided to `chaos resume`/`chaos fork` so they take precedence over any
/// root-level flags. Only overrides fields explicitly set on the subcommand-scoped
/// CLI. Also appends `-c key=value` overrides with highest precedence.
#[cfg(feature = "tui")]
fn merge_interactive_cli_flags(interactive: &mut TuiCli, subcommand_cli: TuiCli) {
    if let Some(profile) = subcommand_cli.config_profile {
        interactive.config_profile = Some(profile);
    }
    if let Some(sandbox) = subcommand_cli.sandbox_mode {
        interactive.sandbox_mode = Some(sandbox);
    }
    if let Some(approval) = subcommand_cli.approval_policy {
        interactive.approval_policy = Some(approval);
    }
    interactive.auto_exec.full_auto |= subcommand_cli.auto_exec.full_auto;
    interactive.auto_exec.headless |= subcommand_cli.auto_exec.headless;
    if let Some(cwd) = subcommand_cli.cwd {
        interactive.cwd = Some(cwd);
    }
    interactive.web_search |= subcommand_cli.web_search;
    interactive.add_dir.extend(subcommand_cli.add_dir);
    interactive.prompt = subcommand_cli.prompt.or(interactive.prompt.take());

    interactive
        .config_overrides
        .raw_overrides
        .extend(subcommand_cli.config_overrides.raw_overrides);
}

fn print_completion(cmd: CompletionCommand) {
    print!("{}", MultitoolCli::completion_script(cmd.shell.into()));
}

#[cfg(test)]
mod tests;
