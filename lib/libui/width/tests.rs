use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn width_suite() {
    display_width_matches_ratatui_layout();
    usable_content_width_refuses_to_return_zero();
}

/// Sound marks carry a cell, wide characters keep theirs, and long lines do
/// not saturate the way a `u16` measurement would.
fn display_width_matches_ratatui_layout() {
    assert_eq!(display_width("ｶﾞﾊﾟ"), 4);
    assert_eq!(display_width("ｶﾞﾞ"), 3);
    assert_eq!(display_width("界ﾞ"), 3);
    assert_eq!(display_width(""), 0);
    assert_eq!(display_width(&"a".repeat(65_536)), 65_536);

    assert_eq!(char_width('\u{FF9E}'), 1);
    assert_eq!(char_width('\u{FF9F}'), 1);
    assert_eq!(char_width('界'), 2);
    assert_eq!(char_width('\u{0301}'), 0);
}

/// The contract is strictly positive: an exhausted or overdrawn budget is
/// `None`, never `Some(0)`.
fn usable_content_width_refuses_to_return_zero() {
    assert_eq!(usable_content_width(0, 0), None);
    assert_eq!(usable_content_width(2, 2), None);
    assert_eq!(usable_content_width(3, 4), None);
    assert_eq!(usable_content_width(5, 4), Some(1));

    assert_eq!(usable_content_width_u16(2, 2), None);
    assert_eq!(usable_content_width_u16(5, 4), Some(1));
}
