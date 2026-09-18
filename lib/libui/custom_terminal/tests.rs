use super::*;
use pretty_assertions::assert_eq;
use ratatui::layout::Rect;
use ratatui::style::Style;

pub(crate) fn custom_terminal_suite() {
    diff_buffers_does_not_emit_clear_to_end_for_full_width_row();
    diff_buffers_clear_to_end_starts_after_wide_char();
}

fn diff_buffers_does_not_emit_clear_to_end_for_full_width_row() {
    let area = Rect::new(0, 0, 3, 2);
    let previous = Buffer::empty(area);
    let mut next = Buffer::empty(area);

    next.cell_mut((2, 0))
        .expect("cell should exist")
        .set_symbol("X");

    let commands = diff_buffers(&previous, &next);

    let clear_count = commands
        .iter()
        .filter(|command| matches!(command, DrawCommand::ClearToEnd { y, .. } if *y == 0))
        .count();
    assert_eq!(
        0, clear_count,
        "expected diff_buffers not to emit ClearToEnd; commands: {commands:?}",
    );
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Put { x: 2, y: 0, .. })),
        "expected diff_buffers to update the final cell; commands: {commands:?}",
    );
}

fn diff_buffers_clear_to_end_starts_after_wide_char() {
    let area = Rect::new(0, 0, 10, 1);
    let mut previous = Buffer::empty(area);
    let mut next = Buffer::empty(area);

    previous.set_string(0, 0, "中文", Style::default());
    next.set_string(0, 0, "中", Style::default());

    let commands = diff_buffers(&previous, &next);
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, DrawCommand::ClearToEnd { x: 2, y: 0, .. })),
        "expected clear-to-end to start after the remaining wide char; commands: {commands:?}"
    );
}
