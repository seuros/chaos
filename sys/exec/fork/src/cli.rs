use chaos_getopt::CliConfigOverrides;
use std::path::PathBuf;

#[derive(usage::Args, Debug, Default)]
pub struct Cli {
    /// Action to perform. If omitted, runs a new non-interactive session.
    #[usage(subcommand)]
    pub command: Option<Command>,

    /// Select the sandbox policy to use when executing model-generated shell
    /// commands.
    #[usage(long = "sandbox", short = 's', value_enum)]
    pub sandbox_mode: Option<chaos_getopt::SandboxModeCliArg>,

    /// Configuration profile from config.toml to specify default options.
    #[usage(long = "profile", short = 'p')]
    pub config_profile: Option<String>,

    /// Override the model used for this exec run.
    #[usage(long = "model", short = 'm', value_name = "MODEL")]
    pub model: Option<String>,

    #[usage(flatten)]
    pub auto_exec: chaos_getopt::GlobalAutoExecFlags,

    /// Tell the agent to use the specified directory as its working root.
    #[usage(long = "cd", short = 'C', value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Allow running Chaos outside a Git repository.
    #[usage(long = "skip-git-repo-check", global)]
    pub skip_git_repo_check: bool,

    /// Additional directories that should be writable alongside the primary workspace.
    #[usage(
        long = "add-dir",
        value_name = "DIR",
        value_hint = usage::ValueHint::DirPath
    )]
    pub add_dir: Vec<PathBuf>,

    /// Run without persisting session history.
    #[usage(long = "ephemeral", global)]
    pub ephemeral: bool,

    /// Internal: start from a serialized Stop-hook transcript snapshot.
    #[usage(long = "fork-snapshot", value_name = "FILE", hide)]
    pub fork_snapshot: Option<PathBuf>,

    /// Path to a JSON Schema file describing the model's final response shape.
    #[usage(long = "output-schema", value_name = "FILE")]
    pub output_schema: Option<PathBuf>,

    #[usage(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Specifies color settings for use in the output.
    #[usage(long = "color", value_enum, default = "auto")]
    pub color: Color,

    /// Force cursor-based progress updates in exec mode.
    #[usage(long = "progress-cursor")]
    pub progress_cursor: bool,

    /// Print events to stdout as JSONL.
    #[usage(long = "json", alias = "experimental-json", global)]
    pub json: bool,

    /// Specifies file where the last message from the agent should be written.
    #[usage(long = "output-last-message", short = 'o', value_name = "FILE", global)]
    pub last_message_file: Option<PathBuf>,

    /// Initial instructions for the agent. If not provided as an argument (or
    /// if `-` is used), instructions are read from stdin.
    #[usage(value_name = "PROMPT", value_hint = usage::ValueHint::Other)]
    pub prompt: Option<String>,
}

impl Cli {
    pub fn normalize(&mut self) {
        if let Some(Command::Resume(args)) = &mut self.command {
            args.normalize();
        }
    }
}

#[derive(Debug, usage::Subcommands)]
pub enum Command {
    /// Resume a previous session by id or pick the most recent with --last.
    Resume(ResumeArgs),

    /// Run a code review against the current repository.
    Review(ReviewArgs),
}

#[derive(usage::Args, Debug)]
pub struct ResumeArgs {
    // Note: This is the direct parser shape. We reinterpret the positional when --last is set
    // so "chaos resume --last <prompt>" treats the positional as a prompt, not a session id.
    /// Conversation/session id (UUID) or process name. UUIDs take precedence if it parses.
    /// If omitted, use --last to pick the most recent recorded session.
    #[usage(value_name = "SESSION_ID")]
    pub session_id: Option<String>,

    /// Resume the most recent recorded session (newest) without specifying an id.
    #[usage(long = "last")]
    pub last: bool,

    /// Show all sessions (disables cwd filtering).
    #[usage(long = "all")]
    pub all: bool,

    /// Prompt to send after resuming the session. If `-` is used, read from stdin.
    #[usage(value_name = "PROMPT", value_hint = usage::ValueHint::Other)]
    pub prompt: Option<String>,
}

impl ResumeArgs {
    fn normalize(&mut self) {
        // When --last is used without an explicit prompt, treat the positional as the prompt.
        if self.last && self.prompt.is_none() {
            self.prompt = self.session_id.take();
        }
    }
}

#[derive(usage::Args, Debug)]
pub struct ReviewArgs {
    /// Review staged, unstaged, and untracked changes.
    #[usage(long = "uncommitted", conflicts("--base", "--commit", "prompt"))]
    pub uncommitted: bool,

    /// Review changes against the given base branch.
    #[usage(
        long = "base",
        value_name = "BRANCH",
        conflicts("--uncommitted", "--commit", "prompt")
    )]
    pub base: Option<String>,

    /// Review the changes introduced by a commit.
    #[usage(
        long = "commit",
        value_name = "SHA",
        conflicts("--uncommitted", "--base", "prompt")
    )]
    pub commit: Option<String>,

    /// Optional commit title to display in the review summary.
    #[usage(long = "title", value_name = "TITLE", requires = "--commit")]
    pub commit_title: Option<String>,

    /// Custom review instructions. If `-` is used, read from stdin.
    #[usage(value_name = "PROMPT", value_hint = usage::ValueHint::Other)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, usage::ValueEnum)]
#[usage(rename_all = "kebab-case")]
pub enum Color {
    Always,
    Never,
    #[default]
    Auto,
}

#[cfg(test)]
#[derive(usage::Cli)]
#[usage(
    bin = "chaos-fork-test",
    multicall,
    unknown_flags = "error",
    args_override_self = false
)]
struct TestRoot {
    #[usage(subcommand)]
    command: TestApplet,
}

#[cfg(test)]
#[derive(usage::Subcommands)]
enum TestApplet {
    #[usage(name = "chaos-exec")]
    ChaosExec(Cli),
}

#[cfg(test)]
pub(crate) fn try_parse_for_test<'v>(args: &[&'v str]) -> Result<Cli, usage::Error<'static, 'v>> {
    use std::ffi::OsStr;

    let argv = args.iter().map(|arg| OsStr::new(*arg)).collect::<Vec<_>>();
    let TestApplet::ChaosExec(mut cli) = TestRoot::try_parse_from(&argv)?.command;
    cli.normalize();
    Ok(cli)
}

#[cfg(test)]
pub(crate) fn parse_owned_for_test(args: &[String]) -> Cli {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    try_parse_for_test(&args).expect("valid exec arguments")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn try_parse<'v>(args: &[&'v str]) -> Result<Cli, usage::Error<'static, 'v>> {
        try_parse_for_test(args)
    }

    fn parse(args: &[&str]) -> Cli {
        try_parse(args).expect("valid exec arguments")
    }

    #[test]
    fn resume_rejects_model_flag_after_subcommand() {
        const PROMPT: &str = "echo resume-with-global-flags-after-subcommand";
        let err = try_parse(&[
            "chaos-exec",
            "resume",
            "--last",
            "--json",
            "--model",
            "gpt-5.4-codex",
            "--headless",
            "--skip-git-repo-check",
            "--ephemeral",
            PROMPT,
        ])
        .expect_err("--model is a root exec override, not a resume flag");

        assert!(matches!(
            err,
            usage::Error::UnknownFlag { .. } | usage::Error::UnexpectedArg { .. }
        ));
    }

    #[test]
    fn resume_parses_prompt_after_global_flags() {
        const PROMPT: &str = "echo resume-with-global-flags-after-subcommand";
        let cli = parse(&[
            "chaos-exec",
            "resume",
            "--last",
            "--json",
            "--headless",
            "--skip-git-repo-check",
            "--ephemeral",
            PROMPT,
        ]);

        assert!(cli.ephemeral);
        let Some(Command::Resume(args)) = cli.command else {
            panic!("expected resume command");
        };
        let effective_prompt = args.prompt.clone().or_else(|| {
            if args.last {
                args.session_id.clone()
            } else {
                None
            }
        });
        assert_eq!(effective_prompt.as_deref(), Some(PROMPT));
    }

    #[test]
    fn resume_accepts_output_last_message_flag_after_subcommand() {
        const PROMPT: &str = "echo resume-with-output-file";
        let cli = parse(&[
            "chaos-exec",
            "resume",
            "session-123",
            "-o",
            "/tmp/resume-output.md",
            PROMPT,
        ]);

        assert_eq!(
            cli.last_message_file,
            Some(PathBuf::from("/tmp/resume-output.md"))
        );
        let Some(Command::Resume(args)) = cli.command else {
            panic!("expected resume command");
        };
        assert_eq!(args.session_id.as_deref(), Some("session-123"));
        assert_eq!(args.prompt.as_deref(), Some(PROMPT));
    }

    #[test]
    fn hidden_fork_snapshot_option_is_parsed() {
        let cli = parse(&[
            "chaos-exec",
            "--fork-snapshot",
            "/tmp/turn-snapshot.json",
            "--ephemeral",
            "reflect",
        ]);

        assert_eq!(
            cli.fork_snapshot,
            Some(PathBuf::from("/tmp/turn-snapshot.json"))
        );
        assert!(cli.ephemeral);
        assert_eq!(cli.prompt.as_deref(), Some("reflect"));
    }
}
