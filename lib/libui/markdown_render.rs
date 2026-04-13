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
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_GFM);
    let parser = Parser::new_ext(input, options);
    let mut w = Writer::new(parser, width, cwd);
    w.run();
    w.text
}

#[cfg(test)]
mod markdown_render_tests {
    include!("markdown_render_tests.rs");
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::text::Text;

    fn lines_to_strings(text: &Text<'_>) -> Vec<String> {
        text.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn wraps_plain_text_when_width_provided() {
        let markdown = "This is a simple sentence that should wrap.";
        let rendered = render_markdown_text_with_width(markdown, Some(16));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "This is a simple".to_string(),
                "sentence that".to_string(),
                "should wrap.".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_list_items_preserving_indent() {
        let markdown = "- first second third fourth";
        let rendered = render_markdown_text_with_width(markdown, Some(14));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec!["- first second".to_string(), "  third fourth".to_string(),]
        );
    }

    #[test]
    fn wraps_nested_lists() {
        let markdown =
            "- outer item with several words to wrap\n  - inner item that also needs wrapping";
        let rendered = render_markdown_text_with_width(markdown, Some(20));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "- outer item with".to_string(),
                "  several words to".to_string(),
                "  wrap".to_string(),
                "    - inner item".to_string(),
                "      that also".to_string(),
                "      needs wrapping".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_ordered_lists() {
        let markdown = "1. ordered item contains many words for wrapping";
        let rendered = render_markdown_text_with_width(markdown, Some(18));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "1. ordered item".to_string(),
                "   contains many".to_string(),
                "   words for".to_string(),
                "   wrapping".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_blockquotes() {
        let markdown = "> block quote with content that should wrap nicely";
        let rendered = render_markdown_text_with_width(markdown, Some(22));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "> block quote with".to_string(),
                "> content that should".to_string(),
                "> wrap nicely".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_blockquotes_inside_lists() {
        let markdown = "- list item\n  > block quote inside list that wraps";
        let rendered = render_markdown_text_with_width(markdown, Some(24));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "- list item".to_string(),
                "  > block quote inside".to_string(),
                "  > list that wraps".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_list_items_containing_blockquotes() {
        let markdown = "1. item with quote\n   > quoted text that should wrap";
        let rendered = render_markdown_text_with_width(markdown, Some(24));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "1. item with quote".to_string(),
                "   > quoted text that".to_string(),
                "   > should wrap".to_string(),
            ]
        );
    }

    #[test]
    fn does_not_wrap_code_blocks() {
        let markdown = "````\nfn main() { println!(\"hi from a long line\"); }\n````";
        let rendered = render_markdown_text_with_width(markdown, Some(10));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec!["fn main() { println!(\"hi from a long line\"); }".to_string(),]
        );
    }

    #[test]
    fn does_not_split_long_url_like_token_without_scheme() {
        let url_like =
            "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
        let rendered = render_markdown_text_with_width(url_like, Some(24));
        let lines = lines_to_strings(&rendered);

        assert_eq!(
            lines.iter().filter(|line| line.contains(url_like)).count(),
            1,
            "expected full URL-like token in one rendered line, got: {lines:?}"
        );
    }

    #[test]
    fn fenced_code_info_string_with_metadata_highlights() {
        for info in &["rust,no_run", "rust no_run", "rust title=\"demo\""] {
            let markdown = format!("```{info}\nfn main() {{}}\n```\n");
            let rendered = render_markdown_text(&markdown);
            let has_rgb = rendered.lines.iter().any(|line| {
                line.spans
                    .iter()
                    .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
            });
            assert!(
                has_rgb,
                "info string \"{info}\" should still produce syntax highlighting"
            );
        }
    }

    #[test]
    fn crlf_code_block_no_extra_blank_lines() {
        let markdown = "```rust\r\nfn main() {}\r\n    line2\r\n```\r\n";
        let rendered = render_markdown_text(markdown);
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec!["fn main() {}".to_string(), "    line2".to_string()],
            "CRLF code block should not produce extra blank lines: {lines:?}"
        );
    }
}
