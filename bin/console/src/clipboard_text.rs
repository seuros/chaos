//! Clipboard text copy support for `/copy` in the TUI.
//!
//! This module owns the policy for getting plain text from the running Codex
//! process into the user's system clipboard. It prefers the direct native
//! clipboard path when the current machine is also the user's desktop, but it
//! intentionally changes strategy in environments where a "local" clipboard
//! would be the wrong one: SSH sessions use OSC 52 so the user's terminal can
//! proxy the copy back to the client.
//!
//! The module is deliberately narrow. It only handles text copy, returns
//! user-facing error strings for the chat UI, and does not try to expose a
//! reusable clipboard abstraction for the rest of the application.
//!
//! The main operational contract is that callers get one best-effort copy
//! attempt and a readable failure message. The selection between native copy
//! and OSC 52 is centralized here so `/copy` does not have to understand
//! platform-specific clipboard behavior.

use base64::Engine as _;
use std::fs::OpenOptions;
use std::io::Write;

/// Copies user-visible text into the most appropriate clipboard for the
/// current environment.
///
/// In a normal desktop session this targets the host clipboard through
/// `arboard`. In SSH sessions it emits an OSC 52 sequence instead, because the
/// process-local clipboard would belong to the remote machine rather than the
/// user's terminal.
///
/// The returned error is intended for display in the TUI rather than for
/// programmatic branching. Callers should treat it as user-facing text. A
/// caller that assumes a specific substring means a stable failure category
/// will be brittle if the fallback policy or wording changes later.
///
/// # Errors
///
/// Returns a descriptive error string when the selected clipboard mechanism is
/// unavailable or the fallback path also fails.
pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return copy_via_osc52(text);
    }

    let error = match arboard::Clipboard::new() {
        Ok(mut clipboard) => match clipboard.set_text(text.to_string()) {
            Ok(()) => return Ok(()),
            Err(err) => format!("clipboard unavailable: {err}"),
        },
        Err(err) => format!("clipboard unavailable: {err}"),
    };

    Err(error)
}

/// Writes text through OSC 52 so the controlling terminal can own the copy.
///
/// This path exists for remote sessions where the process-local clipboard is
/// not the clipboard the user actually wants. It writes directly to the
/// controlling TTY so the escape sequence reaches the terminal even if stdout
/// is redirected.
fn copy_via_osc52(text: &str) -> Result<(), String> {
    let sequence = osc52_sequence(text, std::env::var_os("TMUX").is_some());
    let mut tty = OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .map_err(|e| {
            format!("clipboard unavailable: failed to open /dev/tty for OSC 52 copy: {e}")
        })?;
    tty.write_all(sequence.as_bytes()).map_err(|e| {
        format!("clipboard unavailable: failed to write OSC 52 escape sequence: {e}")
    })?;
    tty.flush().map_err(|e| {
        format!("clipboard unavailable: failed to flush OSC 52 escape sequence: {e}")
    })?;
    Ok(())
}

/// Encodes text as an OSC 52 clipboard sequence.
///
/// When `tmux` is true the sequence is wrapped in the tmux passthrough form so
/// nested terminals still receive the clipboard escape.
fn osc52_sequence(text: &str, tmux: bool) -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(text);
    if tmux {
        format!("\x1bPtmux;\x1b\x1b]52;c;{payload}\x07\x1b\\")
    } else {
        format!("\x1b]52;c;{payload}\x07")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn osc52_sequence_encodes_text_for_terminal_clipboard() {
        assert_eq!(osc52_sequence("hello", false), "\u{1b}]52;c;aGVsbG8=\u{7}");
    }

    #[test]
    fn osc52_sequence_wraps_tmux_passthrough() {
        assert_eq!(
            osc52_sequence("hello", true),
            "\u{1b}Ptmux;\u{1b}\u{1b}]52;c;aGVsbG8=\u{7}\u{1b}\\"
        );
    }
}
