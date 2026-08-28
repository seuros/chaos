use chaos_ipc::product::OS_NAME;
use strum::IntoEnumIterator;
use strum_macros::AsRefStr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use strum_macros::IntoStaticStr;

/// Commands that can be invoked by starting a message with a leading slash.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, EnumIter, AsRefStr, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SlashCommand {
    // DO NOT ALPHA-SORT! Enum order is presentation order in the popup, so
    // more frequently used commands should be listed first.
    Model,
    ContextWindow,
    DynamicEffort,
    Permissions,
    Review,
    Rename,
    New,
    Resume,
    Fork,
    Compact,
    Plan,
    Collab,
    Agent,
    Diff,
    Copy,
    Mention,
    Status,
    Theme,
    Mcp,
    #[strum(serialize = "mcp-add")]
    McpAdd,
    Tools,
    Clamp,
    Accounts,
    Quit,
    Exit,
    Ps,
    #[strum(to_string = "stop", serialize = "clean")]
    Stop,
    Clear,
    #[strum(serialize = "subagents")]
    MultiAgents,
}

impl SlashCommand {
    /// User-visible description shown in the popup.
    pub fn description(self) -> String {
        match self {
            SlashCommand::New => "start a new chat during a conversation".into(),
            SlashCommand::Compact => {
                "summarize conversation to prevent hitting the context limit".into()
            }
            SlashCommand::Review => "review my current changes and find issues".into(),
            SlashCommand::Rename => "rename the current thread".into(),
            SlashCommand::Resume => "resume a saved chat".into(),
            SlashCommand::Clear => "clear the terminal and start a new chat".into(),
            SlashCommand::Fork => "fork the current chat".into(),
            SlashCommand::Quit | SlashCommand::Exit => format!("exit {OS_NAME}"),
            SlashCommand::Diff => "show git diff (including untracked files)".into(),
            SlashCommand::Copy => format!("copy the latest {OS_NAME} output to your clipboard"),
            SlashCommand::Mention => "mention a file".into(),
            SlashCommand::Status => "show current session configuration and token usage".into(),
            SlashCommand::Theme => "choose a syntax highlighting theme".into(),
            SlashCommand::Ps => "list background terminals".into(),
            SlashCommand::Stop => "stop all background terminals".into(),
            SlashCommand::Model => "choose what model and reasoning effort to use".into(),
            SlashCommand::ContextWindow => {
                "choose the ChatGPT context window used for new sessions".into()
            }
            SlashCommand::DynamicEffort => {
                "allow or disallow model-controlled effort changes".into()
            }
            SlashCommand::Plan => "switch to Plan mode".into(),
            SlashCommand::Collab => "change collaboration mode (experimental)".into(),
            SlashCommand::Agent | SlashCommand::MultiAgents => {
                "switch the active agent thread".into()
            }
            SlashCommand::Permissions => format!("choose what {OS_NAME} is allowed to do"),
            SlashCommand::Mcp => "list configured MCP tools".into(),
            SlashCommand::McpAdd => "add a new MCP server".into(),
            SlashCommand::Tools => "show all tools visible to the model".into(),
            SlashCommand::Clamp => "use Claude Code MAX subscription as transport".into(),
            SlashCommand::Accounts => {
                "manage provider accounts and connections (disconnect via CLI)".into()
            }
        }
    }

    /// Command string without the leading '/'. Provided for compatibility with
    /// existing code that expects a method named `command()`.
    pub fn command(self) -> &'static str {
        self.into()
    }

    /// Whether this command supports inline args (for example `/review ...`).
    pub fn supports_inline_args(self) -> bool {
        matches!(
            self,
            SlashCommand::Review
                | SlashCommand::Rename
                | SlashCommand::Plan
                | SlashCommand::ContextWindow
                | SlashCommand::DynamicEffort
        )
    }

    /// Whether this command can be run while a task is in progress.
    pub fn available_during_task(self) -> bool {
        match self {
            SlashCommand::New
            | SlashCommand::Resume
            | SlashCommand::Fork
            | SlashCommand::Compact
            | SlashCommand::Model
            | SlashCommand::ContextWindow
            | SlashCommand::DynamicEffort
            | SlashCommand::Review
            | SlashCommand::Plan
            | SlashCommand::Clear
            | SlashCommand::Accounts => false,
            SlashCommand::Permissions
            | SlashCommand::Diff
            | SlashCommand::Copy
            | SlashCommand::Rename
            | SlashCommand::Mention
            | SlashCommand::Status
            | SlashCommand::Ps
            | SlashCommand::Stop
            | SlashCommand::Mcp
            | SlashCommand::McpAdd
            | SlashCommand::Tools
            | SlashCommand::Quit
            | SlashCommand::Exit => true,
            SlashCommand::Collab => true,
            SlashCommand::Clamp => true,
            SlashCommand::Agent | SlashCommand::MultiAgents => true,
            SlashCommand::Theme => false,
        }
    }

    /// Whether this command stays usable while no account is connected.
    ///
    /// When logged out the model has no usable identity (the active model
    /// resolves to an empty slug), so every command that would touch the
    /// provider is hidden. Only account management and the exits remain so the
    /// user can connect an account or leave.
    pub fn available_when_logged_out(self) -> bool {
        matches!(
            self,
            SlashCommand::Accounts | SlashCommand::Quit | SlashCommand::Exit
        )
    }

    fn is_visible(self) -> bool {
        match self {
            SlashCommand::Clamp => std::process::Command::new("claude")
                .arg("-v")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok(),
            _ => true,
        }
    }
}

/// Return all built-in commands in a Vec paired with their command string.
pub fn built_in_slash_commands() -> Vec<(&'static str, SlashCommand)> {
    SlashCommand::iter()
        .filter(|command| command.is_visible())
        .map(|c| (c.command(), c))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use pretty_assertions::assert_eq;
    use std::str::FromStr;

    use super::SlashCommand;

    pub(crate) fn slash_command_suite() {
        stop_command_is_canonical_name();
        clean_alias_parses_to_stop_command();
        dynamic_effort_accepts_inline_args();
        context_window_accepts_inline_args();
        removed_commands_do_not_parse();
    }
    #[cfg(test)]
    fn stop_command_is_canonical_name() {
        assert_eq!(SlashCommand::Stop.command(), "stop");
    }

    #[cfg(test)]
    fn clean_alias_parses_to_stop_command() {
        assert_eq!(SlashCommand::from_str("clean"), Ok(SlashCommand::Stop));
    }

    #[test]
    fn dynamic_effort_accepts_inline_args() {
        assert_eq!(
            SlashCommand::from_str("dynamic-effort"),
            Ok(SlashCommand::DynamicEffort)
        );
        assert!(SlashCommand::DynamicEffort.supports_inline_args());
    }

    #[test]
    fn context_window_accepts_inline_args() {
        assert_eq!(
            SlashCommand::from_str("context-window"),
            Ok(SlashCommand::ContextWindow)
        );
        assert!(SlashCommand::ContextWindow.supports_inline_args());
    }

    #[test]
    fn removed_commands_do_not_parse() {
        for command in [
            "approvals",
            "setup-default-sandbox",
            "sandbox-add-read-dir",
            "debug-config",
            "login",
            "test-approval",
            "debug-m-drop",
            "debug-m-update",
        ] {
            assert!(
                SlashCommand::from_str(command).is_err(),
                "removed command should not parse: /{command}"
            );
        }
    }
}
