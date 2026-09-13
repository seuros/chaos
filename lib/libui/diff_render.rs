//! Renders unified diffs with line numbers, gutter signs, and optional syntax
//! highlighting.
//!
//! Each `FileChange` variant (Add / Delete / Update) is rendered as a block of
//! diff lines, each prefixed by a right-aligned line number, a gutter sign
//! (`+` / `-` / ` `), and the content text.  When a recognized file extension
//! is present, the content text is syntax-highlighted using
//! [`crate::render::highlight`].
//!
//! **Theme-aware styling:** diff backgrounds adapt to the terminal's
//! background lightness via [`DiffTheme`].  Dark terminals get muted tints
//! (`#212922` green, `#3C170F` red); light terminals get GitHub-style pastels
//! with distinct gutter backgrounds for contrast. The renderer uses fixed
//! palettes for truecolor / 256-color / 16-color terminals so add/delete lines
//! remain visually distinct even when quantizing to limited palettes.
//!
//! **Syntax-theme scope backgrounds:** when the active syntax theme defines
//! background colors for `markup.inserted` / `markup.deleted` (or fallback
//! `diff.inserted` / `diff.deleted`) scopes, those colors override the
//! hardcoded palette for rich color levels.  ANSI-16 mode always uses
//! foreground-only styling regardless of theme scope backgrounds.
//!
//! **Highlighting strategy for `Update` diffs:** the renderer highlights each
//! hunk as a single concatenated block rather than line-by-line.  This
//! preserves syntect's parser state across consecutive lines within a hunk
//! (important for multi-line strings, block comments, etc.).  Cross-hunk state
//! is intentionally *not* preserved because hunks are visually separated and
//! re-synchronize at context boundaries anyway.
//!
//! **Wrapping:** long lines are hard-wrapped at the available column width.
//! Syntax-highlighted spans are split at character boundaries with styles
//! preserved across the split so that no color information is lost.

mod inline;
mod side_by_side;
mod utils;

pub use inline::{
    push_wrapped_diff_line_with_style_context, push_wrapped_diff_line_with_syntax_and_style_context,
};
pub use side_by_side::{
    DiffSummary, calculate_add_remove_from_diff, create_diff_summary, display_path_for,
};
pub use utils::{
    DiffLineType, DiffRenderStyleContext, current_diff_render_style_context, line_number_width,
};

#[cfg(test)]
pub(crate) mod tests;
