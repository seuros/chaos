use pretty_assertions::assert_eq;
use std::str::FromStr;

use super::SlashCommand;

pub(crate) fn slash_command_suite() {
    stop_command_is_canonical_name();
    clean_alias_parses_to_stop_command();
    dynamic_effort_accepts_inline_args();
    context_window_accepts_inline_args();
    clamp_is_discoverable_without_a_claude_installation();
    removed_commands_do_not_parse();
}

#[test]
fn clamp_is_discoverable_without_a_claude_installation() {
    assert!(super::built_in_slash_commands().contains(&("clamp", SlashCommand::Clamp)));
    assert!(SlashCommand::Clamp.supports_inline_args());
    assert!(SlashCommand::Clamp.available_when_logged_out());
    assert!(!SlashCommand::Clamp.available_during_task());
    assert!(SlashCommand::Clamp.description().contains("agy"));
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
