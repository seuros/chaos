use super::*;
use itertools::Itertools as _;
use pretty_assertions::assert_eq;
use ratatui::style::Stylize;
use std::string::ToString;

fn word_wrap_lines_borrowed<'a, I, O>(lines: I, width_or_options: O) -> Vec<Line<'a>>
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

fn concat_line(line: &Line) -> String {
    line.spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>()
}

pub(crate) fn wrapping_suite() {
    trivial_unstyled_no_indents_wide_width();
    simple_unstyled_wrap_narrow_width();
    simple_styled_wrap_preserves_styles();
    with_initial_and_subsequent_indents();
    empty_initial_indent_subsequent_spaces();
    empty_input_yields_single_empty_line();
    leading_spaces_preserved_on_first_line();
    multiple_spaces_between_words_dont_start_next_line_with_spaces();
    break_words_false_allows_overflow_for_long_word();
    hyphen_splitter_breaks_at_hyphen();
    indent_consumes_width_leaving_one_char_space();
    wide_unicode_wraps_by_display_width();
    styled_split_within_span_preserves_style();
    wrap_lines_applies_initial_indent_only_once();
    wrap_lines_without_indents_is_concat_of_single_wraps();
    wrap_lines_borrowed_applies_initial_indent_only_once();
    wrap_lines_borrowed_without_indents_is_concat_of_single_wraps();
    wrap_lines_accepts_borrowed_iterators();
    wrap_lines_accepts_str_slices();
    line_height_counts_double_width_emoji();
    word_wrap_does_not_split_words_simple_english();
    ascii_space_separator_with_no_hyphenation_keeps_url_intact();
    text_contains_url_like_matches_expected_tokens();
    text_contains_url_like_rejects_non_urls();
    line_contains_url_like_checks_across_spans();
    line_has_mixed_url_and_non_url_tokens_detects_prose_plus_url();
    line_has_mixed_url_and_non_url_tokens_ignores_pipe_prefix();
    line_has_mixed_url_and_non_url_tokens_ignores_ordered_list_marker();
    text_contains_url_like_accepts_custom_scheme_with_separator();
    text_contains_url_like_rejects_invalid_ports();
    adaptive_wrap_line_keeps_long_url_like_token_intact();
    adaptive_wrap_line_preserves_default_behavior_for_non_url_tokens();
    adaptive_wrap_line_mixed_line_wraps_long_non_url_token();
    map_owned_wrapped_line_to_range_recovers_on_non_prefix_mismatch();
    map_owned_wrapped_line_to_range_indent_coincides_with_source();
    wrap_ranges_indent_prefix_coincides_with_source_char();
    map_owned_wrapped_line_to_range_repro_overconsumes_repeated_prefix_patterns();
    wrap_ranges_recovers_with_non_space_indents();
    wrap_ranges_trim_handles_owned_lines_with_penalty_char();
}

fn trivial_unstyled_no_indents_wide_width() {
    let line = Line::from("hello");
    let out = word_wrap_line(&line, 10);
    assert_eq!(out.len(), 1);
    assert_eq!(concat_line(&out[0]), "hello");
}

fn simple_unstyled_wrap_narrow_width() {
    let line = Line::from("hello world");
    let out = word_wrap_line(&line, 5);
    assert_eq!(out.len(), 2);
    assert_eq!(concat_line(&out[0]), "hello");
    assert_eq!(concat_line(&out[1]), "world");
}

fn simple_styled_wrap_preserves_styles() {
    let line = Line::from(vec![
        "hello ".fg(crate::theme::error_color()),
        "world".into(),
    ]);
    let out = word_wrap_line(&line, 6);
    assert_eq!(out.len(), 2);
    assert_eq!(concat_line(&out[0]), "hello");
    assert_eq!(out[0].spans.len(), 1);
    assert_eq!(out[0].spans[0].style.fg, Some(crate::theme::error_color()));
    assert_eq!(concat_line(&out[1]), "world");
    assert_eq!(out[1].spans.len(), 1);
    assert_eq!(out[1].spans[0].style.fg, None);
}

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

fn empty_input_yields_single_empty_line() {
    let line = Line::from("");
    let out = word_wrap_line(&line, 10);
    assert_eq!(out.len(), 1);
    assert_eq!(concat_line(&out[0]), "");
}

fn leading_spaces_preserved_on_first_line() {
    let line = Line::from("   hello");
    let out = word_wrap_line(&line, 8);
    assert_eq!(out.len(), 1);
    assert_eq!(concat_line(&out[0]), "   hello");
}

fn multiple_spaces_between_words_dont_start_next_line_with_spaces() {
    let line = Line::from("hello   world");
    let out = word_wrap_line(&line, 8);
    assert_eq!(out.len(), 2);
    assert_eq!(concat_line(&out[0]), "hello");
    assert_eq!(concat_line(&out[1]), "world");
}

fn break_words_false_allows_overflow_for_long_word() {
    let opts = RtOptions::new(5).break_words(false);
    let line = Line::from("supercalifragilistic");
    let out = word_wrap_line(&line, opts);
    assert_eq!(out.len(), 1);
    assert_eq!(concat_line(&out[0]), "supercalifragilistic");
}

fn hyphen_splitter_breaks_at_hyphen() {
    let line = Line::from("hello-world");
    let out = word_wrap_line(&line, 7);
    assert_eq!(out.len(), 2);
    assert_eq!(concat_line(&out[0]), "hello-");
    assert_eq!(concat_line(&out[1]), "world");
}

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

fn wide_unicode_wraps_by_display_width() {
    let line = Line::from("😀😀😀");
    let out = word_wrap_line(&line, 4);
    assert_eq!(out.len(), 2);
    assert_eq!(concat_line(&out[0]), "😀😀");
    assert_eq!(concat_line(&out[1]), "😀");
}

fn styled_split_within_span_preserves_style() {
    use ratatui::style::Stylize;
    let line = Line::from(vec!["abcd".fg(crate::theme::error_color())]);
    let out = word_wrap_line(&line, 2);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].spans.len(), 1);
    assert_eq!(out[1].spans.len(), 1);
    assert_eq!(out[0].spans[0].style.fg, Some(crate::theme::error_color()));
    assert_eq!(out[1].spans[0].style.fg, Some(crate::theme::error_color()));
    assert_eq!(concat_line(&out[0]), "ab");
    assert_eq!(concat_line(&out[1]), "cd");
}

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

fn wrap_lines_without_indents_is_concat_of_single_wraps() {
    let lines = vec![Line::from("hello"), Line::from("world!")];
    let out = word_wrap_lines(lines, 10);
    let rendered: Vec<String> = out.iter().map(concat_line).collect();
    assert_eq!(rendered, vec!["hello", "world!"]);
}

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

fn wrap_lines_borrowed_without_indents_is_concat_of_single_wraps() {
    let lines = [Line::from("hello"), Line::from("world!")];
    let out = word_wrap_lines_borrowed(lines.iter(), 10);
    let rendered: Vec<String> = out.iter().map(concat_line).collect();
    assert_eq!(rendered, vec!["hello", "world!"]);
}

fn wrap_lines_accepts_borrowed_iterators() {
    let lines = [Line::from("hello world"), Line::from("foo bar baz")];
    let out = word_wrap_lines(lines, 10);
    let rendered: Vec<String> = out.iter().map(concat_line).collect();
    assert_eq!(rendered, vec!["hello", "world", "foo bar", "baz"]);
}

fn wrap_lines_accepts_str_slices() {
    let lines = ["hello world", "goodnight moon"];
    let out = word_wrap_lines(lines, 12);
    let rendered: Vec<String> = out.iter().map(concat_line).collect();
    assert_eq!(rendered, vec!["hello world", "goodnight", "moon"]);
}

fn line_height_counts_double_width_emoji() {
    let line = "😀😀😀".into();
    assert_eq!(word_wrap_line(&line, 4).len(), 2);
    assert_eq!(word_wrap_line(&line, 2).len(), 3);
    assert_eq!(word_wrap_line(&line, 6).len(), 1);
}

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

fn line_contains_url_like_checks_across_spans() {
    let line = Line::from(vec![
        "see ".into(),
        "https://example.com/a/very/long/path".fg(crate::theme::accent_color()),
        " for details".into(),
    ]);

    assert!(line_contains_url_like(&line));
}

fn line_has_mixed_url_and_non_url_tokens_detects_prose_plus_url() {
    let line = Line::from("see https://example.com/path for details");
    assert!(line_has_mixed_url_and_non_url_tokens(&line));
}

fn line_has_mixed_url_and_non_url_tokens_ignores_pipe_prefix() {
    let line = Line::from(vec!["  │ ".into(), "https://example.com/path".into()]);
    assert!(!line_has_mixed_url_and_non_url_tokens(&line));
}

fn line_has_mixed_url_and_non_url_tokens_ignores_ordered_list_marker() {
    let line = Line::from("1. https://example.com/path");
    assert!(!line_has_mixed_url_and_non_url_tokens(&line));
}

fn text_contains_url_like_accepts_custom_scheme_with_separator() {
    assert!(text_contains_url_like("myapp://open/some/path"));
}

fn text_contains_url_like_rejects_invalid_ports() {
    assert!(!text_contains_url_like("localhost:99999/path"));
    assert!(!text_contains_url_like("example.com:abc/path"));
}

fn adaptive_wrap_line_keeps_long_url_like_token_intact() {
    let line = Line::from("example.test/a-very-long-path-with-many-segments-and-query?x=1&y=2");
    let out = adaptive_wrap_line(&line, RtOptions::new(20));
    assert_eq!(out.len(), 1);
    assert_eq!(
        concat_line(&out[0]),
        "example.test/a-very-long-path-with-many-segments-and-query?x=1&y=2"
    );
}

fn adaptive_wrap_line_preserves_default_behavior_for_non_url_tokens() {
    let line = Line::from("a_very_long_token_without_spaces_to_force_wrapping");
    let out = adaptive_wrap_line(&line, RtOptions::new(20));
    assert!(
        out.len() > 1,
        "expected non-url token to wrap with default options"
    );
}

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

fn map_owned_wrapped_line_to_range_recovers_on_non_prefix_mismatch() {
    let range = range_mapping::map_owned_wrapped_line_to_range("hello world", 0, "helloX", "");
    assert_eq!(range, 0..5);
}

fn map_owned_wrapped_line_to_range_indent_coincides_with_source() {
    let text = "- item one and some more words";
    let range = range_mapping::map_owned_wrapped_line_to_range(text, 0, "- - item one", "- ");
    assert_eq!(range, 0..10);
}

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
