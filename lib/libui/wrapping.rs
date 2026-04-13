//! Word-wrapping with URL-aware heuristics.
//!
//! The TUI renders text that frequently contains URLs — command output,
//! markdown, agent messages, tool-call results. Standard `textwrap`
//! hyphenation treats `/` and `-` as split points, which breaks URLs
//! across lines and makes them unclickable in terminal emulators.
//!
//! This module provides two wrapping paths:
//!
//! - **Standard** (`word_wrap_line`, `word_wrap_lines`): delegates to
//!   `textwrap` with the caller's options unchanged. Used when the
//!   content is known to be plain prose.
//! - **Adaptive** (`adaptive_wrap_line`, `adaptive_wrap_lines`):
//!   inspects the line for URL-like tokens; if any are found, the
//!   wrapping switches to `AsciiSpace` word separation and a custom
//!   `WordSplitter` that refuses to split URL tokens. Non-URL tokens
//!   on the same line still break at every character boundary (the
//!   custom splitter returns all char indices for non-URL words).
//!
//! Callers that *might* encounter URLs should use the `adaptive_*`
//! functions. Callers that definitely will not (code blocks, pure
//! numeric output) can use the standard path for speed.
//!
//! URL detection is heuristic — see [`text_contains_url_like`] for the
//! rules. False positives suppress hyphenation for that line; false
//! negatives let a URL get split. The heuristic is intentionally
//! conservative: file paths like `src/main.rs` are not matched.

mod custom_splitter;
mod range_mapping;
mod url_detection;

pub use custom_splitter::url_preserving_wrap_options;
pub use range_mapping::{wrap_ranges, wrap_ranges_trim};
pub use url_detection::{
    line_contains_url_like, line_has_mixed_url_and_non_url_tokens, text_contains_url_like,
};

use crate::render::line_utils::push_owned_lines;
use range_mapping::slice_line_spans;
use ratatui::text::Line;
use ratatui::text::Span;
use std::borrow::Cow;
use textwrap::Options;

/// Wraps a single ratatui `Line`, automatically switching to
/// URL-preserving options when the line contains a URL-like token.
///
/// When no URL is detected, wrapping behavior is identical to
/// [`word_wrap_line`]. When a URL is detected, the line is wrapped with
/// [`url_preserving_wrap_options`] — URLs stay intact while non-URL
/// words on the same line still break normally.
#[must_use]
pub fn adaptive_wrap_line<'a>(line: &'a Line<'a>, base: RtOptions<'a>) -> Vec<Line<'a>> {
    let selected = if line_contains_url_like(line) {
        url_preserving_wrap_options(base)
    } else {
        base
    };
    word_wrap_line(line, selected)
}

/// Wraps multiple input lines with URL-aware heuristics, applying
/// `initial_indent` to the first line and `subsequent_indent` to the
/// rest. Each line is independently checked for URLs; URL detection on
/// one line does not affect wrapping of the others.
///
/// This is the multi-line counterpart to [`adaptive_wrap_line`] and is
/// the primary wrapping entry point for most history-cell rendering.
#[allow(private_bounds)]
pub fn adaptive_wrap_lines<'a, I, L>(
    lines: I,
    width_or_options: RtOptions<'a>,
) -> Vec<Line<'static>>
where
    I: IntoIterator<Item = L>,
    L: IntoLineInput<'a>,
{
    let base_opts = width_or_options;
    let mut out: Vec<Line<'static>> = Vec::new();

    for (idx, line) in lines.into_iter().enumerate() {
        let line_input = line.into_line_input();
        let opts = if idx == 0 {
            base_opts.clone()
        } else {
            base_opts
                .clone()
                .initial_indent(base_opts.subsequent_indent.clone())
        };

        let wrapped = adaptive_wrap_line(line_input.as_ref(), opts);
        push_owned_lines(&wrapped, &mut out);
    }

    out
}

#[derive(Debug, Clone)]
pub struct RtOptions<'a> {
    /// The width in columns at which the text will be wrapped.
    pub width: usize,
    /// Line ending used for breaking lines.
    pub line_ending: textwrap::LineEnding,
    /// Indentation used for the first line of output.
    pub initial_indent: Line<'a>,
    /// Indentation used for subsequent lines of output.
    pub subsequent_indent: Line<'a>,
    /// Allow long words to be broken if they cannot fit on a line.
    pub break_words: bool,
    /// Wrapping algorithm to use.
    pub wrap_algorithm: textwrap::WrapAlgorithm,
    /// The line breaking algorithm to use.
    pub word_separator: textwrap::WordSeparator,
    /// The method for splitting words.
    pub word_splitter: textwrap::WordSplitter,
}

impl From<usize> for RtOptions<'_> {
    fn from(width: usize) -> Self {
        RtOptions::new(width)
    }
}

#[allow(dead_code)]
impl<'a> RtOptions<'a> {
    pub fn new(width: usize) -> Self {
        RtOptions {
            width,
            line_ending: textwrap::LineEnding::LF,
            initial_indent: Line::default(),
            subsequent_indent: Line::default(),
            break_words: true,
            word_separator: textwrap::WordSeparator::new(),
            wrap_algorithm: textwrap::WrapAlgorithm::FirstFit,
            word_splitter: textwrap::WordSplitter::HyphenSplitter,
        }
    }

    pub fn line_ending(self, line_ending: textwrap::LineEnding) -> Self {
        RtOptions {
            line_ending,
            ..self
        }
    }

    pub fn width(self, width: usize) -> Self {
        RtOptions { width, ..self }
    }

    pub fn initial_indent(self, initial_indent: Line<'a>) -> Self {
        RtOptions {
            initial_indent,
            ..self
        }
    }

    pub fn subsequent_indent(self, subsequent_indent: Line<'a>) -> Self {
        RtOptions {
            subsequent_indent,
            ..self
        }
    }

    pub fn break_words(self, break_words: bool) -> Self {
        RtOptions {
            break_words,
            ..self
        }
    }

    pub fn word_separator(self, word_separator: textwrap::WordSeparator) -> RtOptions<'a> {
        RtOptions {
            word_separator,
            ..self
        }
    }

    pub fn wrap_algorithm(self, wrap_algorithm: textwrap::WrapAlgorithm) -> RtOptions<'a> {
        RtOptions {
            wrap_algorithm,
            ..self
        }
    }

    pub fn word_splitter(self, word_splitter: textwrap::WordSplitter) -> RtOptions<'a> {
        RtOptions {
            word_splitter,
            ..self
        }
    }
}

#[must_use]
pub fn word_wrap_line<'a, O>(line: &'a Line<'a>, width_or_options: O) -> Vec<Line<'a>>
where
    O: Into<RtOptions<'a>>,
{
    // Flatten the line and record span byte ranges.
    let mut flat = String::new();
    let mut span_bounds = Vec::new();
    let mut acc = 0usize;
    for s in &line.spans {
        let text = s.content.as_ref();
        let start = acc;
        flat.push_str(text);
        acc += text.len();
        span_bounds.push((start..acc, s.style));
    }

    let rt_opts: RtOptions<'a> = width_or_options.into();
    let opts = Options::new(rt_opts.width)
        .line_ending(rt_opts.line_ending)
        .break_words(rt_opts.break_words)
        .wrap_algorithm(rt_opts.wrap_algorithm)
        .word_separator(rt_opts.word_separator)
        .word_splitter(rt_opts.word_splitter);

    let mut out: Vec<Line<'a>> = Vec::new();

    // Compute first line range with reduced width due to initial indent.
    let initial_width_available = opts
        .width
        .saturating_sub(rt_opts.initial_indent.width())
        .max(1);
    let initial_wrapped = wrap_ranges_trim(&flat, opts.clone().width(initial_width_available));
    let Some(first_line_range) = initial_wrapped.first() else {
        return vec![rt_opts.initial_indent.clone()];
    };

    // Build first wrapped line with initial indent.
    let mut first_line = rt_opts.initial_indent.clone().style(line.style);
    {
        let sliced = slice_line_spans(line, &span_bounds, first_line_range);
        let mut spans = first_line.spans;
        spans.append(
            &mut sliced
                .spans
                .into_iter()
                .map(|s| s.patch_style(line.style))
                .collect(),
        );
        first_line.spans = spans;
        out.push(first_line);
    }

    // Wrap the remainder using subsequent indent width and map back to original indices.
    let base = first_line_range.end;
    let skip_leading_spaces = flat[base..].chars().take_while(|c| *c == ' ').count();
    let base = base + skip_leading_spaces;
    let subsequent_width_available = opts
        .width
        .saturating_sub(rt_opts.subsequent_indent.width())
        .max(1);
    let remaining_wrapped = wrap_ranges_trim(&flat[base..], opts.width(subsequent_width_available));
    for r in &remaining_wrapped {
        if r.is_empty() {
            continue;
        }
        let mut subsequent_line = rt_opts.subsequent_indent.clone().style(line.style);
        let offset_range = (r.start + base)..(r.end + base);
        let sliced = slice_line_spans(line, &span_bounds, &offset_range);
        let mut spans = subsequent_line.spans;
        spans.append(
            &mut sliced
                .spans
                .into_iter()
                .map(|s| s.patch_style(line.style))
                .collect(),
        );
        subsequent_line.spans = spans;
        out.push(subsequent_line);
    }

    out
}

/// Utilities to allow wrapping either borrowed or owned lines.
#[derive(Debug)]
enum LineInput<'a> {
    Borrowed(&'a Line<'a>),
    Owned(Line<'a>),
}

impl<'a> LineInput<'a> {
    fn as_ref(&self) -> &Line<'a> {
        match self {
            LineInput::Borrowed(line) => line,
            LineInput::Owned(line) => line,
        }
    }
}

/// This trait makes it easier to pass whatever we need into word_wrap_lines.
trait IntoLineInput<'a> {
    fn into_line_input(self) -> LineInput<'a>;
}

impl<'a> IntoLineInput<'a> for &'a Line<'a> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Borrowed(self)
    }
}

impl<'a> IntoLineInput<'a> for &'a mut Line<'a> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Borrowed(self)
    }
}

impl<'a> IntoLineInput<'a> for Line<'a> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(self)
    }
}

impl<'a> IntoLineInput<'a> for String {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

impl<'a> IntoLineInput<'a> for &'a str {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

impl<'a> IntoLineInput<'a> for Cow<'a, str> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

impl<'a> IntoLineInput<'a> for Span<'a> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

impl<'a> IntoLineInput<'a> for Vec<Span<'a>> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

/// Wrap a sequence of lines, applying the initial indent only to the very first
/// output line, and using the subsequent indent for all later wrapped pieces.
#[allow(private_bounds)]
pub fn word_wrap_lines<'a, I, O, L>(lines: I, width_or_options: O) -> Vec<Line<'static>>
where
    I: IntoIterator<Item = L>,
    L: IntoLineInput<'a>,
    O: Into<RtOptions<'a>>,
{
    let base_opts: RtOptions<'a> = width_or_options.into();
    let mut out: Vec<Line<'static>> = Vec::new();

    for (idx, line) in lines.into_iter().enumerate() {
        let line_input = line.into_line_input();
        let opts = if idx == 0 {
            base_opts.clone()
        } else {
            let mut o = base_opts.clone();
            let sub = o.subsequent_indent.clone();
            o = o.initial_indent(sub);
            o
        };
        let wrapped = word_wrap_line(line_input.as_ref(), opts);
        push_owned_lines(&wrapped, &mut out);
    }

    out
}

#[allow(dead_code)]
pub fn word_wrap_lines_borrowed<'a, I, O>(lines: I, width_or_options: O) -> Vec<Line<'a>>
where
    I: IntoIterator<Item = &'a Line<'a>>,
    O: Into<RtOptions<'a>>,
{
    let base_opts: RtOptions<'a> = width_or_options.into();
    let mut out: Vec<Line<'a>> = Vec::new();
    let mut first = true;
    for line in lines.into_iter() {
        let opts = if first {
            base_opts.clone()
        } else {
            base_opts
                .clone()
                .initial_indent(base_opts.subsequent_indent.clone())
        };
        out.extend(word_wrap_line(line, opts));
        first = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use itertools::Itertools as _;
    use pretty_assertions::assert_eq;
    use ratatui::style::Color;
    use ratatui::style::Stylize;
    use std::string::ToString;

    fn concat_line(line: &Line) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn trivial_unstyled_no_indents_wide_width() {
        let line = Line::from("hello");
        let out = word_wrap_line(&line, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(concat_line(&out[0]), "hello");
    }

    #[test]
    fn simple_unstyled_wrap_narrow_width() {
        let line = Line::from("hello world");
        let out = word_wrap_line(&line, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(concat_line(&out[0]), "hello");
        assert_eq!(concat_line(&out[1]), "world");
    }

    #[test]
    fn simple_styled_wrap_preserves_styles() {
        let line = Line::from(vec!["hello ".red(), "world".into()]);
        let out = word_wrap_line(&line, 6);
        assert_eq!(out.len(), 2);
        assert_eq!(concat_line(&out[0]), "hello");
        assert_eq!(out[0].spans.len(), 1);
        assert_eq!(out[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(concat_line(&out[1]), "world");
        assert_eq!(out[1].spans.len(), 1);
        assert_eq!(out[1].spans[0].style.fg, None);
    }

    #[test]
    fn with_initial_and_subsequent_indents() {
        let opts = RtOptions::new(8)
            .initial_indent(Line::from("- "))
            .subsequent_indent(Line::from("  "));
        let line = Line::from("hello world foo");
        let out = word_wrap_line(&line, opts);
        assert!(concat_line(&out[0]).starts_with("- "));
        assert!(concat_line(&out[1]).starts_with("  "));
        assert!(concat_line(&out[2]).starts_with("  "));
        assert_eq!(concat_line(&out[0]), "- hello");
        assert_eq!(concat_line(&out[1]), "  world");
        assert_eq!(concat_line(&out[2]), "  foo");
    }

    #[test]
    fn empty_initial_indent_subsequent_spaces() {
        let opts = RtOptions::new(8)
            .initial_indent(Line::from(""))
            .subsequent_indent(Line::from("    "));
        let line = Line::from("hello world foobar");
        let out = word_wrap_line(&line, opts);
        assert!(concat_line(&out[0]).starts_with("hello"));
        for l in &out[1..] {
            assert!(concat_line(l).starts_with("    "));
        }
    }

    #[test]
    fn empty_input_yields_single_empty_line() {
        let line = Line::from("");
        let out = word_wrap_line(&line, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(concat_line(&out[0]), "");
    }

    #[test]
    fn leading_spaces_preserved_on_first_line() {
        let line = Line::from("   hello");
        let out = word_wrap_line(&line, 8);
        assert_eq!(out.len(), 1);
        assert_eq!(concat_line(&out[0]), "   hello");
    }

    #[test]
    fn multiple_spaces_between_words_dont_start_next_line_with_spaces() {
        let line = Line::from("hello   world");
        let out = word_wrap_line(&line, 8);
        assert_eq!(out.len(), 2);
        assert_eq!(concat_line(&out[0]), "hello");
        assert_eq!(concat_line(&out[1]), "world");
    }

    #[test]
    fn break_words_false_allows_overflow_for_long_word() {
        let opts = RtOptions::new(5).break_words(false);
        let line = Line::from("supercalifragilistic");
        let out = word_wrap_line(&line, opts);
        assert_eq!(out.len(), 1);
        assert_eq!(concat_line(&out[0]), "supercalifragilistic");
    }

    #[test]
    fn hyphen_splitter_breaks_at_hyphen() {
        let line = Line::from("hello-world");
        let out = word_wrap_line(&line, 7);
        assert_eq!(out.len(), 2);
        assert_eq!(concat_line(&out[0]), "hello-");
        assert_eq!(concat_line(&out[1]), "world");
    }

    #[test]
    fn indent_consumes_width_leaving_one_char_space() {
        let opts = RtOptions::new(4)
            .initial_indent(Line::from(">>>>"))
            .subsequent_indent(Line::from("--"));
        let line = Line::from("hello");
        let out = word_wrap_line(&line, opts);
        assert_eq!(out.len(), 3);
        assert_eq!(concat_line(&out[0]), ">>>>h");
        assert_eq!(concat_line(&out[1]), "--el");
        assert_eq!(concat_line(&out[2]), "--lo");
    }

    #[test]
    fn wide_unicode_wraps_by_display_width() {
        let line = Line::from("😀😀😀");
        let out = word_wrap_line(&line, 4);
        assert_eq!(out.len(), 2);
        assert_eq!(concat_line(&out[0]), "😀😀");
        assert_eq!(concat_line(&out[1]), "😀");
    }

    #[test]
    fn styled_split_within_span_preserves_style() {
        use ratatui::style::Stylize;
        let line = Line::from(vec!["abcd".red()]);
        let out = word_wrap_line(&line, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].spans.len(), 1);
        assert_eq!(out[1].spans.len(), 1);
        assert_eq!(out[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(out[1].spans[0].style.fg, Some(Color::Red));
        assert_eq!(concat_line(&out[0]), "ab");
        assert_eq!(concat_line(&out[1]), "cd");
    }

    #[test]
    fn wrap_lines_applies_initial_indent_only_once() {
        let opts = RtOptions::new(8)
            .initial_indent(Line::from("- "))
            .subsequent_indent(Line::from("  "));

        let lines = vec![Line::from("hello world"), Line::from("foo bar baz")];
        let out = word_wrap_lines(lines, opts);

        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert!(rendered[0].starts_with("- "));
        for r in rendered.iter().skip(1) {
            assert!(r.starts_with("  "));
        }
    }

    #[test]
    fn wrap_lines_without_indents_is_concat_of_single_wraps() {
        let lines = vec![Line::from("hello"), Line::from("world!")];
        let out = word_wrap_lines(lines, 10);
        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert_eq!(rendered, vec!["hello", "world!"]);
    }

    #[test]
    fn wrap_lines_borrowed_applies_initial_indent_only_once() {
        let opts = RtOptions::new(8)
            .initial_indent(Line::from("- "))
            .subsequent_indent(Line::from("  "));

        let lines = [Line::from("hello world"), Line::from("foo bar baz")];
        let out = word_wrap_lines_borrowed(lines.iter(), opts);

        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert!(rendered.first().unwrap().starts_with("- "));
        for r in rendered.iter().skip(1) {
            assert!(r.starts_with("  "));
        }
    }

    #[test]
    fn wrap_lines_borrowed_without_indents_is_concat_of_single_wraps() {
        let lines = [Line::from("hello"), Line::from("world!")];
        let out = word_wrap_lines_borrowed(lines.iter(), 10);
        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert_eq!(rendered, vec!["hello", "world!"]);
    }

    #[test]
    fn wrap_lines_accepts_borrowed_iterators() {
        let lines = [Line::from("hello world"), Line::from("foo bar baz")];
        let out = word_wrap_lines(lines, 10);
        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert_eq!(rendered, vec!["hello", "world", "foo bar", "baz"]);
    }

    #[test]
    fn wrap_lines_accepts_str_slices() {
        let lines = ["hello world", "goodnight moon"];
        let out = word_wrap_lines(lines, 12);
        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert_eq!(rendered, vec!["hello world", "goodnight", "moon"]);
    }

    #[test]
    fn line_height_counts_double_width_emoji() {
        let line = "😀😀😀".into();
        assert_eq!(word_wrap_line(&line, 4).len(), 2);
        assert_eq!(word_wrap_line(&line, 2).len(), 3);
        assert_eq!(word_wrap_line(&line, 6).len(), 1);
    }

    #[test]
    fn word_wrap_does_not_split_words_simple_english() {
        let sample = "Years passed, and Willowmere thrived in peace and friendship. Mira's herb garden flourished with both ordinary and enchanted plants, and travelers spoke of the kindness of the woman who tended them.";
        let line = Line::from(sample);
        let lines = [line];
        let wrapped = word_wrap_lines_borrowed(&lines, 40);
        let joined: String = wrapped.iter().map(ToString::to_string).join("\n");
        assert_eq!(
            joined,
            r#"Years passed, and Willowmere thrived in
peace and friendship. Mira's herb garden
flourished with both ordinary and
enchanted plants, and travelers spoke of
the kindness of the woman who tended
them."#
        );
    }

    #[test]
    fn ascii_space_separator_with_no_hyphenation_keeps_url_intact() {
        let line = Line::from(
            "http://example.com/long-url-with-dashes-wider-than-terminal-window/blah-blah-blah-text/more-gibberish-text",
        );
        let opts = RtOptions::new(24)
            .word_separator(textwrap::WordSeparator::AsciiSpace)
            .word_splitter(textwrap::WordSplitter::NoHyphenation)
            .break_words(false);

        let out = word_wrap_line(&line, opts);

        assert_eq!(out.len(), 1);
        assert_eq!(
            concat_line(&out[0]),
            "http://example.com/long-url-with-dashes-wider-than-terminal-window/blah-blah-blah-text/more-gibberish-text"
        );
    }

    #[test]
    fn text_contains_url_like_matches_expected_tokens() {
        let positives = [
            "https://example.com/a/b",
            "ftp://host/path",
            "www.example.com/path?x=1",
            "example.test/path#frag",
            "localhost:3000/api",
            "127.0.0.1:8080/health",
            "(https://example.com/wrapped-in-parens)",
        ];

        for text in positives {
            assert!(
                text_contains_url_like(text),
                "expected URL-like match for {text:?}"
            );
        }
    }

    #[test]
    fn text_contains_url_like_rejects_non_urls() {
        let negatives = [
            "src/main.rs",
            "foo/bar",
            "key:value",
            "just-some-text-with-dashes",
            "hello.world",
        ];

        for text in negatives {
            assert!(
                !text_contains_url_like(text),
                "did not expect URL-like match for {text:?}"
            );
        }
    }

    #[test]
    fn line_contains_url_like_checks_across_spans() {
        let line = Line::from(vec![
            "see ".into(),
            "https://example.com/a/very/long/path".cyan(),
            " for details".into(),
        ]);

        assert!(line_contains_url_like(&line));
    }

    #[test]
    fn line_has_mixed_url_and_non_url_tokens_detects_prose_plus_url() {
        let line = Line::from("see https://example.com/path for details");
        assert!(line_has_mixed_url_and_non_url_tokens(&line));
    }

    #[test]
    fn line_has_mixed_url_and_non_url_tokens_ignores_pipe_prefix() {
        let line = Line::from(vec!["  │ ".into(), "https://example.com/path".into()]);
        assert!(!line_has_mixed_url_and_non_url_tokens(&line));
    }

    #[test]
    fn line_has_mixed_url_and_non_url_tokens_ignores_ordered_list_marker() {
        let line = Line::from("1. https://example.com/path");
        assert!(!line_has_mixed_url_and_non_url_tokens(&line));
    }

    #[test]
    fn text_contains_url_like_accepts_custom_scheme_with_separator() {
        assert!(text_contains_url_like("myapp://open/some/path"));
    }

    #[test]
    fn text_contains_url_like_rejects_invalid_ports() {
        assert!(!text_contains_url_like("localhost:99999/path"));
        assert!(!text_contains_url_like("example.com:abc/path"));
    }

    #[test]
    fn adaptive_wrap_line_keeps_long_url_like_token_intact() {
        let line = Line::from("example.test/a-very-long-path-with-many-segments-and-query?x=1&y=2");
        let out = adaptive_wrap_line(&line, RtOptions::new(20));
        assert_eq!(out.len(), 1);
        assert_eq!(
            concat_line(&out[0]),
            "example.test/a-very-long-path-with-many-segments-and-query?x=1&y=2"
        );
    }

    #[test]
    fn adaptive_wrap_line_preserves_default_behavior_for_non_url_tokens() {
        let line = Line::from("a_very_long_token_without_spaces_to_force_wrapping");
        let out = adaptive_wrap_line(&line, RtOptions::new(20));
        assert!(
            out.len() > 1,
            "expected non-url token to wrap with default options"
        );
    }

    #[test]
    fn adaptive_wrap_line_mixed_line_wraps_long_non_url_token() {
        let long_non_url = "a_very_long_token_without_spaces_to_force_wrapping";
        let line = Line::from(format!("see https://ex.com {long_non_url}"));
        let out = adaptive_wrap_line(&line, RtOptions::new(24));

        assert!(
            out.iter()
                .any(|line| concat_line(line).contains("https://ex.com")),
            "expected URL token to remain present, got: {out:?}"
        );
        assert!(
            !out.iter()
                .any(|line| concat_line(line).contains(long_non_url)),
            "expected long non-url token to wrap on mixed lines, got: {out:?}"
        );
    }

    #[test]
    fn map_owned_wrapped_line_to_range_recovers_on_non_prefix_mismatch() {
        let range = range_mapping::map_owned_wrapped_line_to_range("hello world", 0, "helloX", "");
        assert_eq!(range, 0..5);
    }

    #[test]
    fn map_owned_wrapped_line_to_range_indent_coincides_with_source() {
        let text = "- item one and some more words";
        let range = range_mapping::map_owned_wrapped_line_to_range(text, 0, "- - item one", "- ");
        assert_eq!(range, 0..10);
    }

    #[test]
    fn wrap_ranges_indent_prefix_coincides_with_source_char() {
        let text = "- first item is long enough to wrap around";
        let opts = || {
            textwrap::Options::new(16)
                .initial_indent("- ")
                .subsequent_indent("- ")
        };
        let ranges = wrap_ranges(text, opts());
        assert!(!ranges.is_empty());

        let mut rebuilt = String::new();
        let mut cursor = 0usize;
        for range in ranges {
            let start = range.start.max(cursor).min(text.len());
            let end = range.end.min(text.len());
            if start < end {
                rebuilt.push_str(&text[start..end]);
            }
            cursor = cursor.max(end);
        }
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn map_owned_wrapped_line_to_range_repro_overconsumes_repeated_prefix_patterns() {
        let text = "- - foo";
        let opts = textwrap::Options::new(3)
            .initial_indent("- ")
            .subsequent_indent("- ")
            .word_separator(textwrap::WordSeparator::AsciiSpace)
            .break_words(false);
        let wrapped = textwrap::wrap(text, opts);
        let Some(line) = wrapped.first() else {
            panic!("expected at least one wrapped line");
        };

        let mapped = range_mapping::map_owned_wrapped_line_to_range(text, 0, line.as_ref(), "- ");
        let expected_len = line
            .as_ref()
            .strip_prefix("- ")
            .unwrap_or(line.as_ref())
            .len();
        let mapped_len = mapped.end.saturating_sub(mapped.start);
        assert!(
            mapped_len <= expected_len,
            "overconsumed source: text={text:?} line={line:?} mapped={mapped:?} expected_len={expected_len}"
        );
    }

    #[test]
    fn wrap_ranges_recovers_with_non_space_indents() {
        let text = "The quick brown fox jumps over the lazy dog";
        let wrapped = textwrap::wrap(
            text,
            textwrap::Options::new(12)
                .initial_indent("* ")
                .subsequent_indent("  "),
        );
        assert!(
            wrapped
                .iter()
                .any(|line| matches!(line, std::borrow::Cow::Owned(_))),
            "expected textwrap to produce owned lines with synthetic indent prefixes"
        );

        let ranges = wrap_ranges(
            text,
            textwrap::Options::new(12)
                .initial_indent("* ")
                .subsequent_indent("  "),
        );
        assert!(!ranges.is_empty());

        let mut rebuilt = String::new();
        let mut cursor = 0usize;
        for range in ranges {
            let start = range.start.max(cursor).min(text.len());
            let end = range.end.min(text.len());
            if start < end {
                rebuilt.push_str(&text[start..end]);
            }
            cursor = cursor.max(end);
        }

        assert_eq!(rebuilt, text);
    }

    #[test]
    fn wrap_ranges_trim_handles_owned_lines_with_penalty_char() {
        fn split_every_char(word: &str) -> Vec<usize> {
            word.char_indices().skip(1).map(|(idx, _)| idx).collect()
        }

        let text = "a_very_long_token_without_spaces";
        let opts = Options::new(8)
            .word_separator(textwrap::WordSeparator::AsciiSpace)
            .word_splitter(textwrap::WordSplitter::Custom(split_every_char))
            .break_words(false);

        let ranges = wrap_ranges_trim(text, opts);
        let rebuilt = ranges
            .iter()
            .map(|range| &text[range.clone()])
            .collect::<String>();

        assert_eq!(rebuilt, text);
        assert!(ranges.len() > 1, "expected wrapped ranges, got: {ranges:?}");
    }
}
