use super::detect_backend;
use super::supports_osc9;
use chaos_kern::config::types::NotificationMethod;
use chaos_sysinfo::terminal::{Multiplexer, TerminalInfo, TerminalName};

fn terminal(name: TerminalName, multiplexer: Option<Multiplexer>) -> TerminalInfo {
    TerminalInfo {
        name,
        term_program: None,
        version: None,
        term: None,
        multiplexer,
    }
}

pub(crate) fn notifications_suite() {
    selects_osc9_method();
    selects_bel_method();
    auto_prefers_bel_without_hints();
    auto_uses_osc9_for_known_terminals();
    auto_prefers_bel_inside_tmux();
}
#[cfg(test)]
fn selects_osc9_method() {
    std::assert_matches!(
        detect_backend(NotificationMethod::Osc9),
        super::DesktopNotificationBackend::Osc9(_)
    );
}

#[cfg(test)]
fn selects_bel_method() {
    std::assert_matches!(
        detect_backend(NotificationMethod::Bel),
        super::DesktopNotificationBackend::Bel(_)
    );
}

fn auto_prefers_bel_without_hints() {
    assert!(!supports_osc9(&terminal(TerminalName::Unknown, None)));
    assert!(!supports_osc9(&terminal(TerminalName::AppleTerminal, None)));
}

fn auto_uses_osc9_for_known_terminals() {
    for name in [
        TerminalName::Iterm2,
        TerminalName::Ghostty,
        TerminalName::WezTerm,
        TerminalName::Kitty,
    ] {
        assert!(supports_osc9(&terminal(name, None)), "{name:?}");
    }
}

fn auto_prefers_bel_inside_tmux() {
    let tmux = Multiplexer::Tmux {
        version: None,
        pane: Some("%1".to_string()),
    };
    assert!(!supports_osc9(&terminal(TerminalName::Ghostty, Some(tmux))));
    assert!(supports_osc9(&terminal(
        TerminalName::Ghostty,
        Some(Multiplexer::Zellij { pane: None })
    )));
}
