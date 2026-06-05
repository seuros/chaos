//! Shared helpers for filtering and matching built-in slash commands.
//!
//! The same sandbox- and feature-gating rules are used by both the composer
//! and the command popup. Centralizing them here keeps those call sites small
//! and ensures they stay in sync.
use std::str::FromStr;

use chaos_glob::fuzzy_match;

use crate::slash_command::SlashCommand;
use crate::slash_command::built_in_slash_commands;

#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinCommandFlags {
    pub collaboration_modes_enabled: bool,
    pub allow_elevate_sandbox: bool,
    /// No account is connected, so only logged-out-safe commands are offered.
    pub login_required: bool,
}

/// Return the built-ins that should be visible/usable for the current input.
pub fn builtins_for_input(flags: BuiltinCommandFlags) -> Vec<(&'static str, SlashCommand)> {
    built_in_slash_commands()
        .into_iter()
        .filter(|(_, cmd)| !flags.login_required || cmd.available_when_logged_out())
        .filter(|(_, cmd)| flags.allow_elevate_sandbox || *cmd != SlashCommand::ElevateSandbox)
        .filter(|(_, cmd)| {
            flags.collaboration_modes_enabled
                || !matches!(*cmd, SlashCommand::Collab | SlashCommand::Plan)
        })
        .collect()
}

/// Find a single built-in command by exact name, after applying the gating rules.
pub fn find_builtin_command(name: &str, flags: BuiltinCommandFlags) -> Option<SlashCommand> {
    let cmd = SlashCommand::from_str(name).ok()?;
    builtins_for_input(flags)
        .into_iter()
        .any(|(_, visible_cmd)| visible_cmd == cmd)
        .then_some(cmd)
}

/// Whether any visible built-in fuzzily matches the provided prefix.
pub fn has_builtin_prefix(name: &str, flags: BuiltinCommandFlags) -> bool {
    builtins_for_input(flags)
        .into_iter()
        .any(|(command_name, _)| fuzzy_match(command_name, name).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn all_enabled_flags() -> BuiltinCommandFlags {
        BuiltinCommandFlags {
            collaboration_modes_enabled: true,
            allow_elevate_sandbox: true,
            login_required: false,
        }
    }

    #[test]
    fn login_required_hides_all_but_logged_out_safe_commands() {
        let flags = BuiltinCommandFlags {
            login_required: true,
            ..all_enabled_flags()
        };
        let visible: Vec<SlashCommand> = builtins_for_input(flags)
            .into_iter()
            .map(|(_, cmd)| cmd)
            .collect();
        assert!(visible.contains(&SlashCommand::Accounts));
        assert!(visible.iter().all(|cmd| cmd.available_when_logged_out()));
        assert!(!visible.contains(&SlashCommand::Model));
        // /accounts must still resolve when typed so the user can connect.
        assert_eq!(
            find_builtin_command("accounts", flags),
            Some(SlashCommand::Accounts)
        );
        assert_eq!(find_builtin_command("model", flags), None);
    }

    #[test]
    fn debug_command_still_resolves_for_dispatch() {
        let cmd = find_builtin_command("debug-config", all_enabled_flags());
        assert_eq!(cmd, Some(SlashCommand::DebugConfig));
    }

    #[test]
    fn clear_command_resolves_for_dispatch() {
        assert_eq!(
            find_builtin_command("clear", all_enabled_flags()),
            Some(SlashCommand::Clear)
        );
    }

    #[test]
    fn stop_command_resolves_for_dispatch() {
        assert_eq!(
            find_builtin_command("stop", all_enabled_flags()),
            Some(SlashCommand::Stop)
        );
    }

    #[test]
    fn clean_command_alias_resolves_for_dispatch() {
        assert_eq!(
            find_builtin_command("clean", all_enabled_flags()),
            Some(SlashCommand::Stop)
        );
    }
}
