use chaos_getopt::ApprovalModeCliArg;
use chaos_getopt::CliConfigOverrides;
use std::path::PathBuf;

#[derive(usage::Args, Debug, Default)]
pub struct Cli {
    /// Optional user prompt to start the session.
    #[usage(value_name = "PROMPT", value_hint = usage::ValueHint::Other)]
    pub prompt: Option<String>,

    // Internal controls set by the top-level `chaos resume` subcommand.
    // These are not exposed as user flags on the base `chaos` command.
    #[usage(skip)]
    pub resume_picker: bool,

    #[usage(skip)]
    pub resume_last: bool,

    /// Internal: resume a specific recorded session by id (UUID). Set by the
    /// top-level `chaos resume <SESSION_ID>` wrapper; not exposed as a public flag.
    #[usage(skip)]
    pub resume_session_id: Option<String>,

    /// Internal: show all sessions (disables cwd filtering and shows CWD column).
    #[usage(skip)]
    pub resume_show_all: bool,

    // Internal controls set by the top-level `chaos fork` subcommand.
    // These are not exposed as user flags on the base `chaos` command.
    #[usage(skip)]
    pub fork_picker: bool,

    #[usage(skip)]
    pub fork_last: bool,

    /// Internal: fork a specific recorded session by id (UUID). Set by the
    /// top-level `chaos fork <SESSION_ID>` wrapper; not exposed as a public flag.
    #[usage(skip)]
    pub fork_session_id: Option<String>,

    /// Internal: show all sessions (disables cwd filtering and shows CWD column).
    #[usage(skip)]
    pub fork_show_all: bool,

    /// Configuration profile from config.toml to specify default options.
    #[usage(long = "profile", short = 'p')]
    pub config_profile: Option<String>,

    /// Select the sandbox policy to use when executing model-generated shell
    /// commands.
    #[usage(long = "sandbox", short = 's', value_enum)]
    pub sandbox_mode: Option<chaos_getopt::SandboxModeCliArg>,

    /// Configure when the model requires human approval before executing a command.
    #[usage(
        long = "ask-for-approval",
        short = 'a',
        value_enum,
        conflicts = "--headless"
    )]
    pub approval_policy: Option<ApprovalModeCliArg>,

    #[usage(flatten)]
    pub auto_exec: chaos_getopt::AutoExecFlags,

    /// Tell the agent to use the specified directory as its working root.
    #[usage(long = "cd", short = 'C', value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Enable live web search. When enabled, the native Responses `web_search` tool is available to the model (no per‑call approval).
    #[usage(long = "search")]
    pub web_search: bool,

    /// Additional directories that should be writable alongside the primary workspace.
    #[usage(
        long = "add-dir",
        value_name = "DIR",
        value_hint = usage::ValueHint::DirPath
    )]
    pub add_dir: Vec<PathBuf>,

    /// Start the session in Claude Code MAX transport mode (clamp).
    /// Equivalent to running /clamp immediately after launch.
    #[usage(long = "clamp")]
    pub clamp: bool,

    /// Disable alternate screen mode
    ///
    /// Runs the TUI in inline mode, preserving terminal scrollback history. This is useful
    /// in terminal multiplexers like Zellij that follow the xterm spec strictly and disable
    /// scrollback in alternate screen buffers.
    #[usage(long = "no-alt-screen")]
    pub no_alt_screen: bool,

    #[usage(skip)]
    pub config_overrides: CliConfigOverrides,
}
