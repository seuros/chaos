//! Markdown rendering for the TUI transcript.
//!
//! This renderer intentionally treats local file links differently from normal web links. For
//! local paths, the displayed text comes from the destination, not the markdown label, so
//! transcripts show the real file target (including normalized location suffixes) and can shorten
//! absolute paths relative to a known working directory.

mod block_handler;
mod inline_handler;
mod line_utils;
mod styles;
mod writer;

pub use line_utils::{COLON_LOCATION_SUFFIX_RE, HASH_LOCATION_SUFFIX_RE, file_url_for_local_link};

use pulldown_cmark::{Options, Parser};
use ratatui::text::Text;
use std::path::Path;

use writer::Writer;

pub fn render_markdown_text(input: &str) -> Text<'static> {
    render_markdown_text_with_width(input, /*width*/ None)
}

/// Render markdown using the current process working directory for local file-link display.
pub fn render_markdown_text_with_width(input: &str, width: Option<usize>) -> Text<'static> {
    let cwd = std::env::current_dir().ok();
    render_markdown_text_with_width_and_cwd(input, width, cwd.as_deref())
}

/// Render markdown with an explicit working directory for local file links.
///
/// The `cwd` parameter controls how absolute local targets are shortened before display. Passing
/// the session cwd keeps full renders, history cells, and streamed deltas visually aligned even
/// when rendering happens away from the process cwd.
pub fn render_markdown_text_with_width_and_cwd(
    input: &str,
    width: Option<usize>,
    cwd: Option<&Path>,
) -> Text<'static> {
    render_markdown_with_prose_style(input, width, cwd, ratatui::style::Style::default())
}

/// Render role-styled prose without applying that base style to code or markers.
pub(crate) fn render_markdown_with_prose_style(
    input: &str,
    width: Option<usize>,
    cwd: Option<&Path>,
    prose_style: ratatui::style::Style,
) -> Text<'static> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_GFM);
    let parser = Parser::new_ext(input, options);
    let mut w = Writer::new(parser, width, cwd);
    w.prose_style = prose_style;
    w.run();
    w.text
}

#[cfg(test)]
pub(crate) mod markdown_render_tests {
    include!("markdown_render_tests.rs");
}

#[cfg(test)]
pub(crate) mod tests;
