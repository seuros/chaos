//! Markdown rendering for the TUI transcript.
//!
//! This renderer intentionally treats local file links differently from normal web links. For
//! local paths, the displayed text comes from the destination, not the markdown label, so
//! transcripts show the real file target (including normalized location suffixes) and can shorten
//! absolute paths relative to a known working directory.

mod block_handler;
mod extensions;
mod inline_handler;
mod line_utils;
mod styles;
mod table;
mod writer;

pub use line_utils::{COLON_LOCATION_SUFFIX_RE, HASH_LOCATION_SUFFIX_RE, file_url_for_local_link};

use pulldown_cmark::{Event, Options, Parser};
use ratatui::text::Text;
use std::path::Path;

use writer::Writer;

/// A diagram is one atomic stream item, with ordinary styled lines as its
/// fallback. Copying and transcript exports retain the source text.
#[derive(Clone, Debug)]
pub(crate) enum MarkdownPart {
    Line(ratatui::text::Line<'static>),
    Mermaid {
        source: String,
        fallback: Vec<ratatui::text::Line<'static>>,
    },
}

impl MarkdownPart {
    pub(crate) fn lines(&self) -> &[ratatui::text::Line<'static>] {
        match self {
            Self::Line(line) => std::slice::from_ref(line),
            Self::Mermaid { fallback, .. } => fallback,
        }
    }

    pub(crate) fn into_lines(self) -> Vec<ratatui::text::Line<'static>> {
        match self {
            Self::Line(line) => vec![line],
            Self::Mermaid { fallback, .. } => fallback,
        }
    }
}

pub(crate) fn render_stream_parts<'a>(
    events: impl Iterator<Item = Event<'a>>,
    width: Option<usize>,
    cwd: &Path,
) -> Vec<MarkdownPart> {
    let mut w = Writer::new(events, width, Some(cwd));
    w.prose_style = crate::theme::assistant_message();
    w.run();
    let mut blocks = w.mermaid_blocks.into_iter().peekable();
    let mut lines = w.text.lines.into_iter().enumerate();
    let mut out = Vec::new();
    while let Some((index, line)) = lines.next() {
        if let Some((range, source)) = blocks.next_if(|(range, _)| range.start == index) {
            let mut fallback = vec![line];
            fallback.extend(lines.by_ref().take(range.len() - 1).map(|(_, line)| line));
            out.push(MarkdownPart::Mermaid { source, fallback });
        } else {
            out.push(MarkdownPart::Line(line));
        }
    }
    out
}

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
    let parser = Parser::new_ext(input, parser_options());
    render_markdown_events_with_prose_style(parser, width, cwd, prose_style)
}

/// Render already-parsed, balanced events using the same writer as a full document.
///
/// Streaming parses the whole buffer to resolve references, then supplies only
/// complete stable blocks. Re-parsing a source prefix would lose definitions
/// located in a later block and could change both link text and table widths.
pub(crate) fn render_markdown_events_with_prose_style<'a>(
    events: impl Iterator<Item = Event<'a>>,
    width: Option<usize>,
    cwd: Option<&Path>,
    prose_style: ratatui::style::Style,
) -> Text<'static> {
    let mut w = Writer::new(events, width, cwd);
    w.prose_style = prose_style;
    w.run();
    w.text
}

/// Shared with the stream collector so buffering follows the same block grammar.
pub(crate) fn parser_options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_DEFINITION_LIST);
    options.insert(Options::ENABLE_SUPERSCRIPT);
    options.insert(Options::ENABLE_SUBSCRIPT);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    options.insert(Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS);
    options
}

#[cfg(test)]
pub(crate) mod markdown_render_tests {
    include!("markdown_render_tests.rs");
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod backport_tests;

#[cfg(test)]
mod literal_tests;
