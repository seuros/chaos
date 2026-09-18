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
    /// No account is connected, so only logged-out-safe commands are offered.
    pub login_required: bool,
}

/// Return the built-ins that should be visible/usable for the current input.
pub fn builtins_for_input(flags: BuiltinCommandFlags) -> Vec<(&'static str, SlashCommand)> {
    built_in_slash_commands()
        .into_iter()
        .filter(|(_, cmd)| !flags.login_required || cmd.available_when_logged_out())
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
pub(crate) mod tests;
