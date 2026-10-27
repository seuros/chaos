//! Probe once at startup, before EventBroker owns stdin. Graphics queries stay
//! on a temporary alternate screen so they cannot alter native scrollback.

use std::io::{IsTerminal, stdin, stdout};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::time::{Duration, Instant};

use chaos_sysinfo::terminal::{self, Multiplexer, TerminalName};
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

/// Discard probe replies tmux forwards after the picker returns, before they
/// reach the event reader as keystrokes.
fn drain_late_replies(fd: BorrowedFd<'_>, quiet: Duration, limit: Duration) -> usize {
    let deadline = Instant::now() + limit;
    let mut buf = [0u8; 256];
    let mut drained = 0;
    loop {
        let wait = quiet.min(deadline.saturating_duration_since(Instant::now()));
        if wait.is_zero() {
            return drained;
        }
        let mut pollfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // Safety: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut pollfd, 1, wait.as_millis() as libc::c_int) };
        if ready <= 0 || pollfd.revents & libc::POLLIN == 0 {
            return drained;
        }
        // Safety: buf is valid for writes of buf.len() bytes.
        let read = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if read <= 0 {
            return drained;
        }
        drained += read as usize;
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
    if !stdin().is_terminal() || !stdout().is_terminal() {
        return None;
    }
    let terminal = terminal::terminal_info();
    if matches!(
        terminal.multiplexer,
        Some(Multiplexer::Screen { .. } | Multiplexer::Zellij { .. })
    ) {
        return None;
    }
    let tmux = terminal.behind_tmux();
    let remote = terminal::is_ssh_session();
    if !should_probe(terminal.name, tmux, remote) {
        return None;
    }
    execute!(stdout(), EnterAlternateScreen).ok()?;
    // The picker's tmux transport enables allow-passthrough for this pane,
    // wraps queries/transmissions in DCS, and chunks image data for tmux.
    // The picker scopes this change to the current pane.
    let result = Picker::from_query_stdio_with_options(query_options(remote));
    if tmux {
        let quiet = Duration::from_millis(if remote { 150 } else { 50 });
        let drained = drain_late_replies(stdin().as_fd(), quiet, Duration::from_millis(500));
        if drained > 0 {
            tracing::debug!(drained, "discarded late terminal replies after image probe");
        }
    }
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
