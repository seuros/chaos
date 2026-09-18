use super::*;
use pretty_assertions::assert_eq;

fn all_enabled_flags() -> BuiltinCommandFlags {
    BuiltinCommandFlags {
        collaboration_modes_enabled: true,
        login_required: false,
    }
}

pub(crate) fn slash_commands_suite() {
    login_required_hides_all_but_logged_out_safe_commands();
    clear_command_resolves_for_dispatch();
    stop_command_resolves_for_dispatch();
    clean_command_alias_resolves_for_dispatch();
}

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

fn clear_command_resolves_for_dispatch() {
    assert_eq!(
        find_builtin_command("clear", all_enabled_flags()),
        Some(SlashCommand::Clear)
    );
}

fn stop_command_resolves_for_dispatch() {
    assert_eq!(
        find_builtin_command("stop", all_enabled_flags()),
        Some(SlashCommand::Stop)
    );
}

fn clean_command_alias_resolves_for_dispatch() {
    assert_eq!(
        find_builtin_command("clean", all_enabled_flags()),
        Some(SlashCommand::Stop)
    );
}
