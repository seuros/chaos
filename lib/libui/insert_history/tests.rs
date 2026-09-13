use super::*;
#[cfg(feature = "vt100-tests")]
use crate::markdown_render::render_markdown_text;
#[cfg(feature = "vt100-tests")]
use crate::test_backend::VT100Backend;
#[cfg(feature = "vt100-tests")]
use ratatui::layout::Rect;

fn writes_bold_then_regular_spans() {
    use ratatui::style::Stylize;

    let spans = ["A".bold(), "B".into()];

    let mut actual: Vec<u8> = Vec::new();
    write_spans(&mut actual, spans.iter()).unwrap();

    let mut expected: Vec<u8> = Vec::new();
    queue!(
        expected,
        SetAttribute(crossterm::style::Attribute::Bold),
        Print("A"),
        SetAttribute(crossterm::style::Attribute::NormalIntensity),
        Print("B"),
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )
    .unwrap();

    assert_eq!(
        String::from_utf8(actual).unwrap(),
        String::from_utf8(expected).unwrap()
    );
}

pub(crate) fn insert_history_suite() {
    writes_bold_then_regular_spans();
    write_spans_emits_osc8_envelope_for_linked_spans();
    write_spans_skips_osc8_when_disabled();
    #[cfg(feature = "vt100-tests")]
    {
        vt100_blockquote_line_emits_green_fg();
        vt100_blockquote_wrap_preserves_color_on_all_wrapped_lines();
        vt100_colored_prefix_then_plain_text_resets_color();
        vt100_deep_nested_mixed_list_third_level_marker_is_colored();
        vt100_prefixed_url_keeps_prefix_and_url_on_same_row();
        vt100_prefixed_url_like_without_scheme_keeps_prefix_and_token_on_same_row();
        vt100_prefixed_mixed_url_line_wraps_suffix_words_together();
        vt100_unwrapped_url_like_clears_continuation_rows();
        vt100_long_unwrapped_url_does_not_insert_extra_blank_gap_before_content();
    }
}
#[cfg(test)]
fn write_spans_emits_osc8_envelope_for_linked_spans() {
    use ratatui::style::Style;

    let alpha = crate::osc8::register("https://example.com/alpha");
    let beta = crate::osc8::register("https://example.com/beta");

    let linked_alpha = |text: &'static str| Span::styled(text, Style::new().underline_color(alpha));
    let linked_beta = |text: &'static str| Span::styled(text, Style::new().underline_color(beta));

    let spans: Vec<Span<'static>> = vec![
        "pre ".into(),
        linked_alpha("foo"),
        linked_alpha("bar"),
        " zzz ".into(),
        linked_beta("baz"),
        " end".into(),
    ];

    let actual = crate::osc8::with_enabled(true, || {
        let mut buf: Vec<u8> = Vec::new();
        write_spans(&mut buf, spans.iter()).unwrap();
        String::from_utf8(buf).unwrap()
    });

    let open_alpha = "\x1b]8;;https://example.com/alpha\x1b\\";
    let open_beta = "\x1b]8;;https://example.com/beta\x1b\\";
    let close = "\x1b]8;;\x1b\\";

    let text_only = actual
        .replace(open_alpha, "")
        .replace(open_beta, "")
        .replace(close, "");
    assert!(text_only.contains("pre foobar zzz baz end"));

    let alpha_open = actual.find(open_alpha).unwrap();
    let after_alpha = &actual[alpha_open + open_alpha.len()..];
    let alpha_close_rel = after_alpha.find(close).unwrap();
    assert_eq!(&after_alpha[..alpha_close_rel], "foobar");

    let beta_open = actual.find(open_beta).unwrap();
    let after_beta = &actual[beta_open + open_beta.len()..];
    let beta_close_rel = after_beta.find(close).unwrap();
    assert_eq!(&after_beta[..beta_close_rel], "baz");

    assert_eq!(actual.matches(open_alpha).count(), 1);
    assert_eq!(actual.matches(open_beta).count(), 1);
    assert_eq!(actual.matches(close).count(), 2);
}

#[cfg(test)]
fn write_spans_skips_osc8_when_disabled() {
    use ratatui::style::Style;

    let sentinel = crate::osc8::register("https://example.com/disabled");
    let spans: Vec<Span<'static>> = vec![
        "hello ".into(),
        Span::styled("world", Style::new().underline_color(sentinel)),
    ];

    let actual = crate::osc8::with_enabled(false, || {
        let mut buf: Vec<u8> = Vec::new();
        write_spans(&mut buf, spans.iter()).unwrap();
        String::from_utf8(buf).unwrap()
    });

    assert!(!actual.contains("\x1b]8;"));
    assert!(actual.contains("hello world"));
}

#[cfg(feature = "vt100-tests")]
fn vt100_blockquote_line_emits_green_fg() {
    // Set up a small off-screen terminal
    let width: u16 = 40;
    let height: u16 = 10;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    // Place viewport on the last line so history inserts scroll upward
    let viewport = Rect::new(0, height - 1, width, 1);
    term.set_viewport_area(viewport);

    // Build a blockquote-like line: apply line-level green style and prefix "> "
    let mut line: Line<'static> = Line::from(vec!["> ".into(), "Hello world".into()]);
    line = line.style(crate::theme::success_color());
    insert_history_lines(&mut term, vec![line]).expect("Failed to insert history lines in test");

    let mut saw_colored = false;
    'outer: for row in 0..height {
        for col in 0..width {
            if let Some(cell) = term.backend().vt100().screen().cell(row, col)
                && cell.has_contents()
                && cell.fgcolor() != vt100::Color::Default
            {
                saw_colored = true;
                break 'outer;
            }
        }
    }
    assert!(
        saw_colored,
        "expected at least one colored cell in vt100 output"
    );
}

#[cfg(feature = "vt100-tests")]
fn vt100_blockquote_wrap_preserves_color_on_all_wrapped_lines() {
    // Force wrapping by using a narrow viewport width and a long blockquote line.
    let width: u16 = 20;
    let height: u16 = 8;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    // Viewport is the last line so history goes directly above it.
    let viewport = Rect::new(0, height - 1, width, 1);
    term.set_viewport_area(viewport);

    // Create a long blockquote with a distinct prefix and enough text to wrap.
    let mut line: Line<'static> = Line::from(vec![
        "> ".into(),
        "This is a long quoted line that should wrap".into(),
    ]);
    line = line.style(crate::theme::success_color());

    insert_history_lines(&mut term, vec![line]).expect("Failed to insert history lines in test");

    // Parse and inspect the final screen buffer.
    let screen = term.backend().vt100().screen();

    // Collect rows that are non-empty; these should correspond to our wrapped lines.
    let mut non_empty_rows: Vec<u16> = Vec::new();
    for row in 0..height {
        let mut any = false;
        for col in 0..width {
            if let Some(cell) = screen.cell(row, col)
                && cell.has_contents()
                && cell.contents() != "\0"
                && cell.contents() != " "
            {
                any = true;
                break;
            }
        }
        if any {
            non_empty_rows.push(row);
        }
    }

    // Expect at least two rows due to wrapping.
    assert!(
        non_empty_rows.len() >= 2,
        "expected wrapped output to span >=2 rows, got {non_empty_rows:?}",
    );

    // For each non-empty row, ensure all non-space cells are using a non-default fg color.
    for row in non_empty_rows {
        for col in 0..width {
            if let Some(cell) = screen.cell(row, col) {
                let contents = cell.contents();
                if !contents.is_empty() && contents != " " {
                    assert!(
                        cell.fgcolor() != vt100::Color::Default,
                        "expected non-default fg on row {row} col {col}, got {:?}",
                        cell.fgcolor()
                    );
                }
            }
        }
    }
}

#[cfg(feature = "vt100-tests")]
fn vt100_colored_prefix_then_plain_text_resets_color() {
    let width: u16 = 40;
    let height: u16 = 6;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let viewport = Rect::new(0, height - 1, width, 1);
    term.set_viewport_area(viewport);

    // First span colored, rest plain.
    let line: Line<'static> = Line::from(vec![
        Span::styled(
            "1. ",
            ratatui::style::Style::default().fg(crate::theme::dim_color()),
        ),
        Span::raw("Hello world"),
    ]);

    insert_history_lines(&mut term, vec![line]).expect("Failed to insert history lines in test");

    let screen = term.backend().vt100().screen();

    // Find the first non-empty row; verify first three cells are colored, following cells default.
    'rows: for row in 0..height {
        let mut has_text = false;
        for col in 0..width {
            if let Some(cell) = screen.cell(row, col)
                && cell.has_contents()
                && cell.contents() != " "
            {
                has_text = true;
                break;
            }
        }
        if !has_text {
            continue;
        }

        // Expect "1. Hello world" starting at col 0.
        for col in 0..3 {
            let cell = screen.cell(row, col).unwrap();
            assert!(
                cell.fgcolor() != vt100::Color::Default,
                "expected colored prefix at col {col}, got {:?}",
                cell.fgcolor()
            );
        }
        for col in 3..(3 + "Hello world".len() as u16) {
            let cell = screen.cell(row, col).unwrap();
            assert_eq!(
                cell.fgcolor(),
                vt100::Color::Default,
                "expected default color for plain text at col {col}, got {:?}",
                cell.fgcolor()
            );
        }
        break 'rows;
    }
}

#[cfg(feature = "vt100-tests")]
fn vt100_deep_nested_mixed_list_third_level_marker_is_colored() {
    // Markdown with five levels (ordered → unordered → ordered → unordered → unordered).
    let md = "1. First\n   - Second level\n     1. Third level (ordered)\n        - Fourth level (bullet)\n          - Fifth level to test indent consistency\n";
    let text = render_markdown_text(md);
    let lines: Vec<Line<'static>> = text.lines.clone();

    let width: u16 = 60;
    let height: u16 = 12;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let viewport = ratatui::layout::Rect::new(0, height - 1, width, 1);
    term.set_viewport_area(viewport);

    insert_history_lines(&mut term, lines).expect("Failed to insert history lines in test");

    let screen = term.backend().vt100().screen();

    // Reconstruct screen rows as strings to locate the 3rd level line.
    let rows: Vec<String> = screen.rows(0, width).collect();

    let needle = "1. Third level (ordered)";
    let row_idx = rows
        .iter()
        .position(|r| r.contains(needle))
        .unwrap_or_else(|| {
            panic!("expected to find row containing {needle:?}, have rows: {rows:?}")
        });
    let col_start = rows[row_idx].find(needle).unwrap() as u16; // column where '1' starts

    // Verify that the numeric marker ("1.") at the third level is colored
    // (non-default fg) and the content after the following space resets to default.
    for c in [col_start, col_start + 1] {
        let cell = screen.cell(row_idx as u16, c).unwrap();
        assert!(
            cell.fgcolor() != vt100::Color::Default,
            "expected colored 3rd-level marker at row {row_idx} col {c}, got {:?}",
            cell.fgcolor()
        );
    }
    let content_col = col_start + 3; // skip '1', '.', and the space
    if let Some(cell) = screen.cell(row_idx as u16, content_col) {
        assert_eq!(
            cell.fgcolor(),
            vt100::Color::Default,
            "expected default color for 3rd-level content at row {row_idx} col {content_col}, got {:?}",
            cell.fgcolor()
        );
    }
}

#[cfg(feature = "vt100-tests")]
fn vt100_prefixed_url_keeps_prefix_and_url_on_same_row() {
    let width: u16 = 48;
    let height: u16 = 8;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let viewport = Rect::new(0, height - 1, width, 1);
    term.set_viewport_area(viewport);

    let url = "http://a-long-url.com/this/that/blablablab/new.aspx/many_people_like_how";
    let line: Line<'static> = Line::from(vec!["  │ ".into(), url.into()]);

    insert_history_lines(&mut term, vec![line]).expect("insert history");

    let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();

    assert!(
        rows.iter().any(|r| r.contains("│ http://a-long-url.com")),
        "expected prefix and URL on same row, rows: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.trim_end() == "│"),
        "unexpected orphan prefix row, rows: {rows:?}"
    );
}

#[cfg(feature = "vt100-tests")]
fn vt100_prefixed_url_like_without_scheme_keeps_prefix_and_token_on_same_row() {
    let width: u16 = 48;
    let height: u16 = 8;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let viewport = Rect::new(0, height - 1, width, 1);
    term.set_viewport_area(viewport);

    let url_like = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
    let line: Line<'static> = Line::from(vec!["  │ ".into(), url_like.into()]);

    insert_history_lines(&mut term, vec![line]).expect("insert history");

    let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();

    assert!(
        rows.iter()
            .any(|r| r.contains("│ example.test/api/v1/projects")),
        "expected prefix and URL-like token on same row, rows: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.trim_end() == "│"),
        "unexpected orphan prefix row, rows: {rows:?}"
    );
}

#[cfg(feature = "vt100-tests")]
fn vt100_prefixed_mixed_url_line_wraps_suffix_words_together() {
    let width: u16 = 24;
    let height: u16 = 10;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let viewport = Rect::new(0, height - 1, width, 1);
    term.set_viewport_area(viewport);

    let url = "https://example.test/path/abcdef12345";
    let line: Line<'static> = Line::from(vec![
        "  │ ".into(),
        "see ".into(),
        url.into(),
        " tail words".into(),
    ]);

    insert_history_lines(&mut term, vec![line]).expect("insert mixed history");

    let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
    assert!(
        rows.iter().any(|r| r.contains("│ see")),
        "expected prefixed prose before URL, rows: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("tail words")),
        "expected suffix words to wrap as a phrase, rows: {rows:?}"
    );
}

#[cfg(feature = "vt100-tests")]
fn vt100_unwrapped_url_like_clears_continuation_rows() {
    let width: u16 = 20;
    let height: u16 = 10;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let viewport = Rect::new(0, height - 1, width, 1);
    term.set_viewport_area(viewport);

    let filler_line: Line<'static> = Line::from(vec![
        "  │ ".into(),
        "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX".into(),
    ]);
    insert_history_lines(&mut term, vec![filler_line]).expect("insert filler history");

    let url_like = "example.test/api/v1/short";
    let url_line: Line<'static> = Line::from(vec!["  │ ".into(), url_like.into()]);
    insert_history_lines(&mut term, vec![url_line]).expect("insert url-like history");

    let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
    let first_row = rows
        .iter()
        .position(|row| row.contains("│ example.test/api"))
        .unwrap_or_else(|| panic!("expected url-like first row in screen rows: {rows:?}"));
    assert!(
        first_row + 1 < rows.len(),
        "expected a continuation row for wrapped URL-like line, rows: {rows:?}"
    );
    let continuation_row = rows[first_row + 1].trim_end();

    assert!(
        continuation_row.contains("/v1/short") || continuation_row.contains("short"),
        "expected continuation row to contain wrapped URL-like tail, got: {continuation_row:?}"
    );
    assert!(
        !continuation_row.contains('X'),
        "expected continuation row to be cleared before writing wrapped URL-like content, got: {continuation_row:?}"
    );
}

#[cfg(feature = "vt100-tests")]
fn vt100_long_unwrapped_url_does_not_insert_extra_blank_gap_before_content() {
    let width: u16 = 56;
    let height: u16 = 24;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let viewport = Rect::new(0, height - 1, width, 1);
    term.set_viewport_area(viewport);

    let prompt = "Write a long URL as output for testing";
    insert_history_lines(&mut term, vec![Line::from(prompt)]).expect("insert prompt line");

    let long_url = format!(
        "https://example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/{}",
        "very-long-segment-".repeat(16),
    );
    let url_line: Line<'static> = Line::from(vec!["• ".into(), long_url.into()]);
    insert_history_lines(&mut term, vec![url_line]).expect("insert long url line");

    let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
    let prompt_row = rows
        .iter()
        .position(|row| row.contains("Write a long URL as output for testing"))
        .unwrap_or_else(|| panic!("expected prompt row in screen rows: {rows:?}"));
    let url_row = rows
        .iter()
        .position(|row| row.contains("• https://example.test/api"))
        .unwrap_or_else(|| panic!("expected URL first row in screen rows: {rows:?}"));

    assert!(
        url_row <= prompt_row + 2,
        "expected URL content to appear immediately after prompt (allowing at most one spacer row), got prompt_row={prompt_row}, url_row={url_row}, rows={rows:?}",
    );
}
