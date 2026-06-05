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
    Approvals,
    Permissions,
    #[strum(serialize = "setup-default-sandbox")]
    ElevateSandbox,
    #[strum(serialize = "sandbox-add-read-dir")]
    SandboxReadRoot,
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
    DebugConfig,
    Theme,
    Mcp,
    #[strum(serialize = "mcp-add")]
    McpAdd,
    Tools,
    Clamp,
    Accounts,
    Login,
    Quit,
    Exit,
    Ps,
    #[strum(to_string = "stop", serialize = "clean")]
    Stop,
    Clear,
    TestApproval,
    #[strum(serialize = "subagents")]
    MultiAgents,
    // Debugging commands.
    #[strum(serialize = "debug-m-drop")]
    MemoryDrop,
    #[strum(serialize = "debug-m-update")]
    MemoryUpdate,
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
            SlashCommand::DebugConfig => {
                "show config layers and requirement sources for debugging".into()
            }
            SlashCommand::Theme => "choose a syntax highlighting theme".into(),
            SlashCommand::Ps => "list background terminals".into(),
            SlashCommand::Stop => "stop all background terminals".into(),
            SlashCommand::MemoryDrop => "DO NOT USE".into(),
            SlashCommand::MemoryUpdate => "DO NOT USE".into(),
            SlashCommand::Model => "choose what model and reasoning effort to use".into(),
            SlashCommand::Plan => "switch to Plan mode".into(),
            SlashCommand::Collab => "change collaboration mode (experimental)".into(),
            SlashCommand::Agent | SlashCommand::MultiAgents => {
                "switch the active agent thread".into()
            }
            SlashCommand::Approvals | SlashCommand::Permissions => {
                format!("choose what {OS_NAME} is allowed to do")
            }
            SlashCommand::ElevateSandbox => "set up elevated agent sandbox".into(),
            SlashCommand::SandboxReadRoot => {
                "let sandbox read a directory: /sandbox-add-read-dir <absolute_path>".into()
            }
            SlashCommand::Mcp => "list configured MCP tools".into(),
            SlashCommand::McpAdd => "add a new MCP server".into(),
            SlashCommand::Tools => "show all tools visible to the model".into(),
            SlashCommand::Clamp => "use Claude Code MAX subscription as transport".into(),
            SlashCommand::Accounts => {
                "manage provider accounts and connections (disconnect via CLI)".into()
            }
            SlashCommand::Login => "manage provider accounts and connections".into(),
            SlashCommand::TestApproval => "test approval request".into(),
        }
    }

    /// Command string without the leading '/'. Provided for compatibility with
    /// existing code that expects a method named `command()`.
    pub fn command(self) -> &'static str {
        match self {
            SlashCommand::Accounts => "accounts",
            SlashCommand::Login => "login",
            _ => self.into(),
        }
    }

    /// Whether this command supports inline args (for example `/review ...`).
    pub fn supports_inline_args(self) -> bool {
        matches!(
            self,
            SlashCommand::Review
                | SlashCommand::Rename
                | SlashCommand::Plan
                | SlashCommand::SandboxReadRoot
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
            | SlashCommand::ElevateSandbox
            | SlashCommand::SandboxReadRoot
            | SlashCommand::Review
            | SlashCommand::Plan
            | SlashCommand::Clear
            | SlashCommand::Accounts
            | SlashCommand::Login
            | SlashCommand::MemoryDrop
            | SlashCommand::MemoryUpdate => false,
            SlashCommand::Approvals
            | SlashCommand::Permissions
            | SlashCommand::Diff
            | SlashCommand::Copy
            | SlashCommand::Rename
            | SlashCommand::Mention
            | SlashCommand::Status
            | SlashCommand::DebugConfig
            | SlashCommand::Ps
            | SlashCommand::Stop
            | SlashCommand::Mcp
            | SlashCommand::McpAdd
            | SlashCommand::Tools
            | SlashCommand::Quit
            | SlashCommand::Exit => true,
            SlashCommand::TestApproval => true,
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
            SlashCommand::Accounts | SlashCommand::Login | SlashCommand::Quit | SlashCommand::Exit
        )
    }

    fn is_visible(self) -> bool {
        match self {
            SlashCommand::SandboxReadRoot => false,
            SlashCommand::Login => false,
            SlashCommand::Copy => true,
            SlashCommand::TestApproval => cfg!(debug_assertions),
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
mod tests {
    use pretty_assertions::assert_eq;
    use std::str::FromStr;

    use super::SlashCommand;

    #[test]
    fn stop_command_is_canonical_name() {
        assert_eq!(SlashCommand::Stop.command(), "stop");
    }

    #[test]
    fn clean_alias_parses_to_stop_command() {
        assert_eq!(SlashCommand::from_str("clean"), Ok(SlashCommand::Stop));
    }
}
