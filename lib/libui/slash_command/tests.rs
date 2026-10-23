use pretty_assertions::assert_eq;
use std::str::FromStr;

use super::SlashCommand;

fn goal_submits_a_request_but_is_not_available_during_work_or_logout() {
    assert_eq!(SlashCommand::from_str("goal"), Ok(SlashCommand::Goal));
    assert!(SlashCommand::Goal.supports_inline_args());
    assert!(!SlashCommand::Goal.available_during_task());
    assert!(!SlashCommand::Goal.available_when_logged_out());
}

pub(crate) fn slash_command_suite() {
    goal_submits_a_request_but_is_not_available_during_work_or_logout();
    stop_command_is_canonical_name();
    clean_alias_parses_to_stop_command();
    dynamic_effort_accepts_inline_args();
    clamp_is_discoverable_without_a_claude_installation();
    reflex_setup_and_test_are_available_without_a_model_account();
    removed_commands_do_not_parse();
}

fn clamp_is_discoverable_without_a_claude_installation() {
    assert!(super::built_in_slash_commands().contains(&("clamp", SlashCommand::Clamp)));
    assert!(SlashCommand::Clamp.supports_inline_args());
    assert!(SlashCommand::Clamp.available_when_logged_out());
    assert!(!SlashCommand::Clamp.available_during_task());
    assert!(SlashCommand::Clamp.description().contains("agy"));
}

fn reflex_setup_and_test_are_available_without_a_model_account() {
    assert!(super::built_in_slash_commands().contains(&("reflex", SlashCommand::Reflex)));
    assert!(SlashCommand::Reflex.supports_inline_args());
    assert!(SlashCommand::Reflex.available_when_logged_out());
    assert!(!SlashCommand::Reflex.available_during_task());
}
#[cfg(test)]
fn stop_command_is_canonical_name() {
    assert_eq!(SlashCommand::Stop.command(), "stop");
}

#[cfg(test)]
fn clean_alias_parses_to_stop_command() {
    assert_eq!(SlashCommand::from_str("clean"), Ok(SlashCommand::Stop));
}

fn dynamic_effort_accepts_inline_args() {
    for (name, command) in [
        ("dynamic-effort", SlashCommand::DynamicEffort),
        ("context-window", SlashCommand::ContextWindow),
    ] {
        assert_eq!(SlashCommand::from_str(name), Ok(command), "{name}");
        assert!(command.supports_inline_args(), "{name}");
    }
}

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
