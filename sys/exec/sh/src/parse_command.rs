//! Shell command parsing — public facade over the focused submodules.

mod ast;
mod lexer;
mod parser;

pub use lexer::shlex_join;
pub use parser::{extract_shell_command, is_small_formatting_command, parse_command_impl};

use chaos_ipc::parse_command::ParsedCommand;
use parser::single_unknown_for_command;

/// DO NOT REVIEW THIS CODE BY HAND
/// This parsing code is quite complex and not easy to hand-modify.
/// The easiest way to iterate is to add unit tests and have Chaos fix the implementation.
/// The test-only child module is kept in the native `parse_command/tests.rs` file.
///
/// Parses metadata out of an arbitrary command.
/// These commands are model driven and could include just about anything.
/// The parsing is slightly lossy due to the ~infinite expressiveness of an arbitrary command.
/// The goal of the parsed metadata is to be able to provide the user with a human readable gist
/// of what it is doing.
pub fn parse_command(command: &[String]) -> Vec<ParsedCommand> {
    // Parse and then collapse consecutive duplicate commands to avoid redundant summaries.
    let parsed = parse_command_impl(command);
    let mut deduped: Vec<ParsedCommand> = Vec::with_capacity(parsed.len());
    for cmd in parsed.into_iter() {
        if deduped.last().is_some_and(|prev| prev == &cmd) {
            continue;
        }
        deduped.push(cmd);
    }
    if deduped
        .iter()
        .any(|cmd| matches!(cmd, ParsedCommand::Unknown { .. }))
    {
        vec![single_unknown_for_command(command)]
    } else {
        deduped
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
/// Tests encourage using TDD + Chaos to fix the implementation.
mod tests;
