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
use chaos_console::AppExitInfo;
use chaos_console::Cli as TuiCli;
use chaos_console::ExitReason;
use chaos_fork::Cli as ExecCli;
use chaos_fork::Command as ExecCommand;
use chaos_fork::ReviewArgs;
use chaos_getopt::CliConfigOverrides;
use chaos_ipc::product::OS_NAME;
use chaos_selinux::ExecPolicyCheckCommand;
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use supports_color::Stream;

mod debug_logging;
mod mcp_cmd;
mod models_cmd;

use crate::mcp_cmd::McpCli;
use crate::models_cmd::ModelsCli;

use chaos_kern::terminal::TerminalName;

/// Chaos
///
/// If no subcommand is specified, options will be forwarded to the interactive CLI.
#[derive(Debug, usage::Cli)]
#[usage(
    author = env!("CARGO_PKG_AUTHORS"),
    version = concat!(env!("CARGO_PKG_VERSION"), ".", env!("CHAOS_BUILD_TS")),
    // The executable is sometimes invoked via a platform‑specific name like
    // `chaos-x86_64-unknown-linux-musl`, but the help output should always use
    // the generic `chaos` command name that users run.
    bin = "chaos",
    usage = "chaos [OPTIONS] [PROMPT]\n       chaos [OPTIONS] <COMMAND> [ARGS]",
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

    #[usage(flatten)]
    interactive: TuiCli,

    #[usage(subcommand)]
    subcommand: Option<Subcommand>,
}

impl MultitoolCli {
    fn normalize(&mut self) {
        if let Some(Subcommand::Exec(exec)) = &mut self.subcommand {
            exec.normalize();
        }
    }
}

#[derive(Debug, usage::Subcommands)]
enum Subcommand {
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
    Resume(ResumeCommand),

    /// Fork a previous interactive session (picker by default; use --last to fork the most recent).
    Fork(ForkCommand),

    /// Run Chaos as an HTTP trigger server.
    Serve(chaos_httpd::ServeCli),

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

    let MultitoolCli {
        debug,
        provider,
        config_overrides: mut root_config_overrides,
        mut interactive,
        subcommand,
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
        None => {
            prepend_root_flags!(interactive, root_config_overrides);
            let exit_info = run_interactive_tui(interactive, arg0_paths.clone()).await?;
            handle_app_exit(exit_info)?;
        }
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
        Some(Subcommand::Resume(ResumeCommand {
            session_id,
            last,
            all,
            config_overrides,
        })) => {
            interactive = finalize_resume_interactive(
                interactive,
                root_config_overrides.clone(),
                session_id,
                last,
                all,
                config_overrides,
            );
            let exit_info = run_interactive_tui(interactive, arg0_paths.clone()).await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Fork(ForkCommand {
            session_id,
            last,
            all,
            config_overrides,
        })) => {
            interactive = finalize_fork_interactive(
                interactive,
                root_config_overrides.clone(),
                session_id,
                last,
                all,
                config_overrides,
            );
            let exit_info = run_interactive_tui(interactive, arg0_paths.clone()).await?;
            handle_app_exit(exit_info)?;
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
            let profile = interactive.config_profile.clone();
            models_cmd::run(cli, profile, models_provider).await?;
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

async fn run_interactive_tui(
    mut interactive: TuiCli,
    arg0_paths: Arg0DispatchPaths,
) -> std::io::Result<AppExitInfo> {
    if let Some(prompt) = interactive.prompt.take() {
        // Normalize CRLF/CR to LF so CLI-provided text can't leak `\r` into TUI state.
        interactive.prompt = Some(prompt.replace("\r\n", "\n").replace('\r', "\n"));
    }

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

fn confirm(prompt: &str) -> std::io::Result<bool> {
    eprintln!("{prompt}");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Build the final `TuiCli` for a `chaos resume` invocation.
fn finalize_resume_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    session_id: Option<String>,
    last: bool,
    show_all: bool,
    resume_cli: TuiCli,
) -> TuiCli {
    // Start with the parsed interactive CLI so resume shares the same
    // configuration surface area as `chaos` without additional flags.
    let resume_session_id = session_id;
    interactive.resume_picker = resume_session_id.is_none() && !last;
    interactive.resume_last = last;
    interactive.resume_session_id = resume_session_id;
    interactive.resume_show_all = show_all;

    // Merge resume-scoped flags and overrides with highest precedence.
    merge_interactive_cli_flags(&mut interactive, resume_cli);

    // Propagate any root-level config overrides (e.g. `-c key=value`).
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

/// Build the final `TuiCli` for a `chaos fork` invocation.
fn finalize_fork_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    session_id: Option<String>,
    last: bool,
    show_all: bool,
    fork_cli: TuiCli,
) -> TuiCli {
    // Start with the parsed interactive CLI so fork shares the same
    // configuration surface area as `chaos` without additional flags.
    let fork_session_id = session_id;
    interactive.fork_picker = fork_session_id.is_none() && !last;
    interactive.fork_last = last;
    interactive.fork_session_id = fork_session_id;
    interactive.fork_show_all = show_all;

    // Merge fork-scoped flags and overrides with highest precedence.
    merge_interactive_cli_flags(&mut interactive, fork_cli);

    // Propagate any root-level config overrides (e.g. `-c key=value`).
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

/// Merge flags provided to `chaos resume`/`chaos fork` so they take precedence over any
/// root-level flags. Only overrides fields explicitly set on the subcommand-scoped
/// CLI. Also appends `-c key=value` overrides with highest precedence.
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
    if subcommand_cli.auto_exec.full_auto {
        interactive.auto_exec.full_auto = true;
    }
    if subcommand_cli.auto_exec.headless {
        interactive.auto_exec.headless = true;
    }
    if let Some(cwd) = subcommand_cli.cwd {
        interactive.cwd = Some(cwd);
    }
    if subcommand_cli.web_search {
        interactive.web_search = true;
    }
    if !subcommand_cli.add_dir.is_empty() {
        interactive.add_dir.extend(subcommand_cli.add_dir);
    }
    if let Some(prompt) = subcommand_cli.prompt {
        // Normalize CRLF/CR to LF so CLI-provided text can't leak `\r` into TUI state.
        interactive.prompt = Some(prompt.replace("\r\n", "\n").replace('\r', "\n"));
    }

    interactive
        .config_overrides
        .raw_overrides
        .extend(subcommand_cli.config_overrides.raw_overrides);
}

fn print_completion(cmd: CompletionCommand) {
    print!("{}", MultitoolCli::completion_script(cmd.shell.into()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use chaos_ipc::ProcessId;
    use chaos_ipc::protocol::TokenUsage;
    use pretty_assertions::assert_eq;
    use std::ffi::OsStr;

    fn try_parse_cli<'v>(args: &[&'v str]) -> Result<MultitoolCli, usage::Error<'static, 'v>> {
        let argv = args.iter().map(|arg| OsStr::new(*arg)).collect::<Vec<_>>();
        let mut cli = MultitoolCli::try_parse_from(&argv)?;
        cli.normalize();
        Ok(cli)
    }

    fn finalize_resume_from_args(args: &[&str]) -> TuiCli {
        let cli = try_parse_cli(args).expect("parse");
        let MultitoolCli {
            debug: _,
            interactive,
            config_overrides: root_overrides,
            subcommand,
            provider: _,
        } = cli;

        let Subcommand::Resume(ResumeCommand {
            session_id,
            last,
            all,
            config_overrides: resume_cli,
        }) = subcommand.expect("resume present")
        else {
            unreachable!()
        };

        finalize_resume_interactive(
            interactive,
            root_overrides,
            session_id,
            last,
            all,
            resume_cli,
        )
    }

    fn finalize_fork_from_args(args: &[&str]) -> TuiCli {
        let cli = try_parse_cli(args).expect("parse");
        let MultitoolCli {
            debug: _,
            interactive,
            config_overrides: root_overrides,
            subcommand,
            provider: _,
        } = cli;

        let Subcommand::Fork(ForkCommand {
            session_id,
            last,
            all,
            config_overrides: fork_cli,
        }) = subcommand.expect("fork present")
        else {
            unreachable!()
        };

        finalize_fork_interactive(interactive, root_overrides, session_id, last, all, fork_cli)
    }

    #[test]
    fn cli_parser_and_exit_format_suite() {
        exec_resume_cli_parses_positionals_and_subcommand_flags();
        auto_exec_flags_do_not_leak_to_unrelated_subcommands();
        format_exit_messages_handles_zero_usage_resume_color_and_thread_names();
        resume_and_fork_picker_logic_cover_default_last_session_and_all_modes();
        resume_merges_subcommand_scoped_flags_with_highest_precedence();
        debug_flag_is_global_and_defaults_false();
        mcp_add_transport_shapes_and_constraints_are_preserved();
        completion_shells_and_global_provider_are_preserved();
        global_config_order_and_duplicate_scalar_rejection_are_preserved();
    }

    fn exec_resume_cli_parses_positionals_and_subcommand_flags() {
        let cli = try_parse_cli(&["chaos", "exec", "--json", "resume", "--last", "2+2"])
            .expect("parse should succeed");
        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(chaos_fork::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };
        assert!(args.last);
        assert_eq!(args.session_id, None);
        assert_eq!(args.prompt.as_deref(), Some("2+2"));

        let cli = try_parse_cli(&[
            "chaos",
            "exec",
            "resume",
            "session-123",
            "-o",
            "/tmp/resume-output.md",
            "re-review",
        ])
        .expect("parse should succeed");
        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(chaos_fork::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };
        assert_eq!(
            exec.last_message_file,
            Some(std::path::PathBuf::from("/tmp/resume-output.md"))
        );
        assert_eq!(args.session_id.as_deref(), Some("session-123"));
        assert_eq!(args.prompt.as_deref(), Some("re-review"));

        let cli = try_parse_cli(&[
            "chaos",
            "exec",
            "resume",
            "--last",
            "--headless",
            "continue",
        ])
        .expect("parse should succeed");
        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(chaos_fork::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };
        assert!(exec.auto_exec.headless);
        assert!(args.last);
        assert_eq!(args.prompt.as_deref(), Some("continue"));
    }

    fn auto_exec_flags_do_not_leak_to_unrelated_subcommands() {
        for args in [
            &["chaos", "accounts", "--headless"][..],
            &["chaos", "accounts", "--full-auto"][..],
            &["chaos", "completion", "--headless"][..],
            &["chaos", "completion", "--full-auto"][..],
        ] {
            let err = try_parse_cli(args).expect_err("parse should fail");
            assert!(matches!(
                err,
                usage::Error::UnknownFlag { .. } | usage::Error::UnexpectedArg { .. }
            ));
        }
    }

    fn sample_exit_info(conversation_id: Option<&str>, process_name: Option<&str>) -> AppExitInfo {
        let token_usage = TokenUsage {
            output_tokens: 2,
            total_tokens: 2,
            ..Default::default()
        };
        AppExitInfo {
            token_usage,
            process_id: conversation_id
                .map(ProcessId::from_string)
                .map(Result::unwrap),
            process_name: process_name.map(str::to_string),
            exit_reason: ExitReason::UserRequested,
        }
    }

    fn format_exit_messages_handles_zero_usage_resume_color_and_thread_names() {
        let exit_info = AppExitInfo {
            token_usage: TokenUsage::default(),
            process_id: None,
            process_name: None,
            exit_reason: ExitReason::UserRequested,
        };
        let lines = format_exit_messages(exit_info, false);
        assert!(lines.is_empty());

        let exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        let lines = format_exit_messages(exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session, run chaos resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );

        let exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        let lines = format_exit_messages(exit_info, true);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("\u{1b}[36m"));

        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            Some("my-thread"),
        );
        let lines = format_exit_messages(exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session by name, run chaos resume my-thread".to_string(),
                "Or by session ID, run chaos resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    fn resume_and_fork_picker_logic_cover_default_last_session_and_all_modes() {
        let interactive = finalize_resume_from_args(["chaos", "resume"].as_ref());
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
        assert!(!interactive.resume_show_all);

        let interactive = finalize_resume_from_args(["chaos", "resume", "--last"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
        assert!(!interactive.resume_show_all);

        let interactive = finalize_resume_from_args(["chaos", "resume", "1234"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id.as_deref(), Some("1234"));
        assert!(!interactive.resume_show_all);

        let interactive = finalize_resume_from_args(["chaos", "resume", "--all"].as_ref());
        assert!(interactive.resume_picker);
        assert!(interactive.resume_show_all);

        let interactive = finalize_fork_from_args(["chaos", "fork"].as_ref());
        assert!(interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_session_id, None);
        assert!(!interactive.fork_show_all);

        let interactive = finalize_fork_from_args(["chaos", "fork", "--last"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(interactive.fork_last);
        assert_eq!(interactive.fork_session_id, None);
        assert!(!interactive.fork_show_all);

        let interactive = finalize_fork_from_args(["chaos", "fork", "1234"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_session_id.as_deref(), Some("1234"));
        assert!(!interactive.fork_show_all);

        let interactive = finalize_fork_from_args(["chaos", "fork", "--all"].as_ref());
        assert!(interactive.fork_picker);
        assert!(interactive.fork_show_all);
    }

    fn resume_merges_subcommand_scoped_flags_with_highest_precedence() {
        let interactive = finalize_resume_from_args(
            [
                "chaos",
                "resume",
                "sid",
                "--full-auto",
                "--search",
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "interactive",
                "-p",
                "my-profile",
                "-C",
                "/tmp",
            ]
            .as_ref(),
        );

        assert_eq!(interactive.config_profile.as_deref(), Some("my-profile"));
        assert_matches!(
            interactive.sandbox_mode,
            Some(chaos_getopt::SandboxModeCliArg::WorkspaceWrite)
        );
        assert_matches!(
            interactive.approval_policy,
            Some(chaos_getopt::ApprovalModeCliArg::Interactive)
        );
        assert!(interactive.auto_exec.full_auto);
        assert_eq!(
            interactive.cwd.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );
        assert!(interactive.web_search);
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id.as_deref(), Some("sid"));

        let interactive = finalize_resume_from_args(["chaos", "resume", "--headless"].as_ref());
        assert!(interactive.auto_exec.headless);
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
    }

    fn debug_flag_is_global_and_defaults_false() {
        for args in [
            &["chaos", "--debug"][..],
            &["chaos", "-d"][..],
            &["chaos", "--debug", "exec", "say hi"][..],
            &["chaos", "exec", "--debug", "say hi"][..],
        ] {
            let cli = try_parse_cli(args).expect("parse");
            assert!(cli.debug, "debug should be enabled for {args:?}");
        }

        let cli = try_parse_cli(&["chaos"]).expect("parse");
        assert!(!cli.debug);
    }

    fn mcp_add_transport_shapes_and_constraints_are_preserved() {
        let cli = try_parse_cli(&[
            "chaos",
            "mcp",
            "add",
            "local",
            "--env",
            "TOKEN=secret",
            "--",
            "node",
            "server.js",
        ])
        .expect("stdio transport should parse");
        let Some(Subcommand::Mcp(mcp)) = cli.subcommand else {
            panic!("expected mcp subcommand");
        };
        let crate::mcp_cmd::McpSubcommand::Add(add) = mcp.subcommand else {
            panic!("expected mcp add");
        };
        assert_eq!(add.name, "local");
        assert_eq!(add.transport_args.command, ["node", "server.js"]);
        assert_eq!(add.transport_args.env.len(), 1);
        assert_eq!(add.transport_args.env[0].0, "TOKEN");
        assert_eq!(add.transport_args.env[0].1, "secret");
        assert_eq!(add.transport_args.url, None);

        let cli = try_parse_cli(&[
            "chaos",
            "mcp",
            "add",
            "remote",
            "--url",
            "https://example.test/mcp",
            "--bearer-token-env-var",
            "MCP_TOKEN",
        ])
        .expect("HTTP transport should parse");
        let Some(Subcommand::Mcp(mcp)) = cli.subcommand else {
            panic!("expected mcp subcommand");
        };
        let crate::mcp_cmd::McpSubcommand::Add(add) = mcp.subcommand else {
            panic!("expected mcp add");
        };
        assert!(add.transport_args.command.is_empty());
        assert_eq!(
            add.transport_args.url.as_deref(),
            Some("https://example.test/mcp")
        );
        assert_eq!(
            add.transport_args.bearer_token_env_var.as_deref(),
            Some("MCP_TOKEN")
        );

        for args in [
            &["chaos", "mcp", "add", "missing"][..],
            &[
                "chaos",
                "mcp",
                "add",
                "both",
                "--url",
                "https://example.test/mcp",
                "--",
                "node",
            ][..],
            &[
                "chaos",
                "mcp",
                "add",
                "remote",
                "--url",
                "https://example.test/mcp",
                "--env",
                "A=B",
            ][..],
            &[
                "chaos",
                "mcp",
                "add",
                "local",
                "--bearer-token",
                "secret",
                "--",
                "node",
            ][..],
        ] {
            assert!(
                try_parse_cli(args).is_err(),
                "invalid MCP transport should fail: {args:?}"
            );
        }
    }

    fn completion_shells_and_global_provider_are_preserved() {
        let cli = try_parse_cli(&["chaos", "completion"]).expect("default shell should parse");
        let Some(Subcommand::Completion(completion)) = cli.subcommand else {
            panic!("expected completion command");
        };
        assert_eq!(completion.shell, CompletionShell::Bash);

        for (name, expected) in [
            ("nushell", CompletionShell::Nushell),
            ("nu", CompletionShell::Nushell),
            ("powershell", CompletionShell::PowerShell),
            ("pwsh", CompletionShell::PowerShell),
        ] {
            let cli =
                try_parse_cli(&["chaos", "completion", name]).expect("shell alias should parse");
            let Some(Subcommand::Completion(completion)) = cli.subcommand else {
                panic!("expected completion command");
            };
            assert_eq!(completion.shell, expected);
        }

        let script = MultitoolCli::completion_script(usage::complete::Shell::Bash);
        assert!(!script.is_empty());
        assert!(script.contains("chaos"));

        for args in [
            &["chaos", "--provider", "anthropic", "models"][..],
            &["chaos", "models", "--provider", "anthropic", "--refresh"][..],
        ] {
            let cli = try_parse_cli(args).expect("global provider should parse in either position");
            assert_eq!(cli.provider.as_deref(), Some("anthropic"));
            let Some(Subcommand::Models(models)) = cli.subcommand else {
                panic!("expected models command");
            };
            assert_eq!(models.refresh, args.contains(&"--refresh"));
        }
    }

    fn global_config_order_and_duplicate_scalar_rejection_are_preserved() {
        let cli = try_parse_cli(&[
            "chaos",
            "-c",
            "model=first",
            "mcp",
            "-c",
            "model=second",
            "list",
        ])
        .expect("global config flags should parse around subcommands");
        assert_eq!(
            cli.config_overrides.raw_overrides,
            ["model=first", "model=second"]
        );

        assert!(
            try_parse_cli(&["chaos", "--provider", "openai", "--provider", "anthropic",]).is_err()
        );
    }
}
