use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn clipboard_text_suite() {
    osc52_sequence_encodes_text_for_terminal_clipboard();
    osc52_sequence_wraps_tmux_passthrough();
}

fn osc52_sequence_encodes_text_for_terminal_clipboard() {
    assert_eq!(osc52_sequence("hello", false), "\u{1b}]52;c;aGVsbG8=\u{7}");
}

fn osc52_sequence_wraps_tmux_passthrough() {
    assert_eq!(
        osc52_sequence("hello", true),
        "\u{1b}Ptmux;\u{1b}\u{1b}]52;c;aGVsbG8=\u{7}\u{1b}\\"
    );
}
