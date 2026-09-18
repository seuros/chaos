use super::*;

#[test]
fn title_without_icons_preserves_existing_behavior() {
    assert_eq!(
        terminal_title_text(
            Some("Compaction Control"),
            TerminalTitleState::Idle,
            None,
            None,
            None,
        ),
        "Compaction Control"
    );
    assert_eq!(
        terminal_title_text(None, TerminalTitleState::Idle, None, None, None),
        "new session"
    );
}

#[test]
fn working_icon_replaces_idle_icon_and_falls_back_when_absent() {
    assert_eq!(
        terminal_title_text(
            Some("Terminal Icons"),
            TerminalTitleState::Working,
            Some("✦"),
            Some("◒"),
            None,
        ),
        "◒ Terminal Icons"
    );
    assert_eq!(
        terminal_title_text(
            Some("Terminal Icons"),
            TerminalTitleState::Working,
            Some("✦"),
            None,
            None,
        ),
        "✦ Terminal Icons"
    );
}

#[test]
fn attention_precedence_is_ready_for_follow_up() {
    assert_eq!(
        terminal_title_text(
            Some("Terminal Icons"),
            TerminalTitleState::Attention,
            Some("✦"),
            Some("◒"),
            Some("●"),
        ),
        "● Terminal Icons"
    );
}
