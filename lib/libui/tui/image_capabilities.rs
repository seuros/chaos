//! Probe once at startup, before EventBroker owns stdin. Graphics queries stay
//! on a temporary alternate screen so they cannot alter native scrollback.

use std::io::{IsTerminal, stdin, stdout};

use chaos_kern::terminal::{Multiplexer, TerminalName};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Capability, Picker, ProtocolType};

fn should_probe(name: TerminalName, tmux: bool, remote: bool) -> bool {
    // tmux and SSH often expose only tmux-256color or xterm-256color.
    // These identify the transport; probing discovers the outer terminal's capabilities.
    tmux || remote
        || matches!(
            name,
            TerminalName::Kitty
                | TerminalName::Ghostty
                | TerminalName::Iterm2
                | TerminalName::WezTerm
        )
}

fn query_options(remote: bool) -> QueryStdioOptions {
    QueryStdioOptions {
        timeout_ms: if remote { 1500 } else { 500 },
        // Flat diagram images compress well; encoding runs off the UI thread.
        kitty_compression: remote,
        // Inline transmission works when the renderer and terminal run on
        // different machines, including through a local tmux.
        kitty_shared_memory_object: None,
        ..Default::default()
    }
}

fn protocol_confirmed(protocol: ProtocolType, capabilities: &[Capability]) -> bool {
    match protocol {
        ProtocolType::Kitty => capabilities.contains(&Capability::Kitty),
        ProtocolType::Sixel => capabilities.contains(&Capability::Sixel),
        // iTerm2 is inferred from environment hints, which can become stale
        // across SSH or tmux client reattachments. Prefer reported capabilities.
        ProtocolType::Iterm2 | ProtocolType::Halfblocks => false,
    }
}

pub(super) fn image_capabilities() -> Option<Picker> {
    if !stdin().is_terminal()
        || !stdout().is_terminal()
        || std::env::var_os("STY").is_some()
        || std::env::var_os("ZELLIJ").is_some()
    {
        return None;
    }
    let terminal = chaos_kern::terminal::terminal_info();
    let tmux = matches!(terminal.multiplexer, Some(Multiplexer::Tmux { .. }))
        || std::env::var("TERM").is_ok_and(|term| term.starts_with("tmux"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|program| program == "tmux");
    let remote =
        std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    if !should_probe(terminal.name, tmux, remote) {
        return None;
    }
    execute!(stdout(), EnterAlternateScreen).ok()?;
    // The picker's tmux transport enables allow-passthrough for this pane,
    // wraps queries/transmissions in DCS, and chunks image data for tmux.
    // The picker scopes this change to the current pane.
    let result = Picker::from_query_stdio_with_options(query_options(remote));
    // Restore on success and failure, before any normal TUI output.
    execute!(stdout(), LeaveAlternateScreen).ok()?;
    result.ok().filter(|picker| {
        picker.protocol_type() != ProtocolType::Halfblocks
            && picker.font_size().width > 0
            && picker.font_size().height > 0
            && (!tmux || picker.tmux_detected())
            && (!(tmux || remote)
                || protocol_confirmed(picker.protocol_type(), picker.capabilities()))
    })
}

#[cfg(test)]
mod tests;
