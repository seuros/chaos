use crate::app_event::AppEvent;

use super::ChatWidget;

const DEFAULT_TERMINAL_TITLE: &str = "new session";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalTitleState {
    Idle,
    Working,
    #[allow(dead_code)]
    Attention,
}

impl ChatWidget {
    pub(super) fn refresh_terminal_title(&self) {
        if self.config.terminal_title == chaos_kern::config::TerminalTitleMode::Off {
            return;
        }
        let state = if self.agent_turn_running || self.mcp_startup_status.is_some() {
            TerminalTitleState::Working
        } else {
            TerminalTitleState::Idle
        };
        let title = terminal_title_text(
            self.process_name.as_deref(),
            state,
            self.config.tui_terminal_title_icon.as_deref(),
            self.config.tui_terminal_title_working_icon.as_deref(),
            /*attention_icon*/ None,
        );
        self.app_event_tx
            .send(AppEvent::SetTerminalTitle(Some(title)));
    }
}

fn terminal_title_text(
    process_name: Option<&str>,
    state: TerminalTitleState,
    idle_icon: Option<&str>,
    working_icon: Option<&str>,
    attention_icon: Option<&str>,
) -> String {
    let process_name = process_name.unwrap_or(DEFAULT_TERMINAL_TITLE);
    let icon = match state {
        TerminalTitleState::Idle => idle_icon,
        TerminalTitleState::Working => working_icon.or(idle_icon),
        TerminalTitleState::Attention => attention_icon.or(working_icon).or(idle_icon),
    };
    match icon {
        Some(icon) => format!("{icon} {process_name}"),
        None => process_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
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
}
