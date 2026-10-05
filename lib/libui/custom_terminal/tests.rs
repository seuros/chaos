use super::*;
use pretty_assertions::assert_eq;
use ratatui::layout::Rect;
use ratatui::style::Style;

pub(crate) fn custom_terminal_suite() {
    diff_buffers_does_not_emit_clear_to_end_for_full_width_row();
    diff_buffers_clear_to_end_starts_after_wide_char();
    graphics_cells_use_declared_width_and_protect_skipped_cells();
}

fn graphics_cells_use_declared_width_and_protect_skipped_cells() {
    use std::num::NonZeroU16;
    let area = Rect::new(3, 2, 6, 2);
    let previous = Buffer::empty(area);
    let mut next = Buffer::empty(area);
    next[(3, 2)]
        .set_symbol("\x1b_Gsynthetic-image-payload\x1b\\")
        .set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
    for x in 4..7 {
        next[(x, 2)].set_diff_option(CellDiffOption::Skip);
    }
    next[(7, 2)].set_symbol("X");
    next[(3, 3)].set_symbol("next row");
    let commands = diff_buffers(&previous, &next);
    assert!(
        commands
            .iter()
            .any(|cmd| matches!(cmd, DrawCommand::Put { x: 3, y: 2, .. }))
    );
    assert!(
        commands
            .iter()
            .any(|cmd| matches!(cmd, DrawCommand::Put { x: 7, y: 2, .. }))
    );
    assert!(
        commands
            .iter()
            .any(|cmd| matches!(cmd, DrawCommand::Put { x: 3, y: 3, .. }))
    );
    assert!(
        !commands
            .iter()
            .any(|cmd| matches!(cmd, DrawCommand::ClearToEnd { x, y: 2, .. } if *x < 8))
    );
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
