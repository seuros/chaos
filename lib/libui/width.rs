//! Terminal display-width helpers and guards for fixed prefix columns.
//!
//! `unicode-width` 0.2.3 accounts for halfwidth katakana sound marks itself,
//! so [`display_width`] and [`char_width`] delegate to its measurements without
//! adding cells that ratatui already counts.
//!
//! Several render paths reserve fixed columns for bullets,
//! gutters, or labels before laying out content. On a narrow terminal those
//! reserved columns can consume the whole width. [`usable_content_width`]
//! centralises the subtraction and returns `None` rather than zero, so callers
//! must decide what a prefix-only fallback looks like instead of asking the
//! wrapper to lay out text at zero columns.

use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// Returns the display width ratatui uses for terminal text.
///
/// Keep `usize` precision until rendering, including lines longer than
/// `u16::MAX` columns.
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Returns a scalar's terminal width, treating halfwidth sound marks as visible.
pub fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
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
pub(crate) mod tests;
