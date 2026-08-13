//! Terminal display-width helpers and guards for fixed prefix columns.
//!
//! Two problems live here.
//!
//! The first is that `unicode-width` reports zero for the halfwidth katakana
//! sound marks `U+FF9E` and `U+FF9F`, but ratatui lays them out as one cell
//! each. Any code that measures a string with the crate directly and then
//! hands it to ratatui disagrees with the renderer by one column per mark,
//! which shows up as truncation that cuts a cell short or a wrap that
//! overflows. [`display_width`] and [`char_width`] measure the way ratatui
//! draws.
//!
//! The second is that several render paths reserve fixed columns for bullets,
//! gutters, or labels before laying out content. On a narrow terminal those
//! reserved columns can consume the whole width. [`usable_content_width`]
//! centralises the subtraction and returns `None` rather than zero, so callers
//! must decide what a prefix-only fallback looks like instead of asking the
//! wrapper to lay out text at zero columns.

use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// Halfwidth katakana voiced and semi-voiced sound marks.
///
/// `unicode-width` classifies these as combining marks with zero width;
/// ratatui gives them a cell.
const HALFWIDTH_SOUND_MARKS: [char; 2] = ['\u{FF9E}', '\u{FF9F}'];

fn is_halfwidth_sound_mark(ch: char) -> bool {
    HALFWIDTH_SOUND_MARKS.contains(&ch)
}

/// Returns the display width ratatui uses for terminal text.
///
/// Unlike ratatui's own `Line::width`, this keeps `usize` precision, so lines
/// longer than `u16::MAX` columns measure correctly instead of saturating.
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
        + text
            .chars()
            .filter(|ch| is_halfwidth_sound_mark(*ch))
            .count()
}

/// Returns a scalar's terminal width, treating halfwidth sound marks as visible.
pub fn char_width(ch: char) -> usize {
    if is_halfwidth_sound_mark(ch) {
        1
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(0)
    }
}

/// Returns the usable content width after reserving fixed columns.
///
/// Returns `Some(n)` with `n > 0`, or `None` when the reserved columns consume
/// the full width. Treat `None` as "render the prefix alone"; coercing it to
/// zero and wrapping anyway yields empty or unstable output at narrow widths.
pub fn usable_content_width(total_width: usize, reserved_cols: usize) -> Option<usize> {
    total_width
        .checked_sub(reserved_cols)
        .filter(|remaining| *remaining > 0)
}

/// `u16` convenience wrapper around [`usable_content_width`].
///
/// Terminal dimensions arrive as `u16`; the answer is a `usize` because that is
/// what the wrapping layer takes.
pub fn usable_content_width_u16(total_width: u16, reserved_cols: u16) -> Option<usize> {
    usable_content_width(usize::from(total_width), usize::from(reserved_cols))
}

#[cfg(test)]
pub(crate) mod tests {
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
}
