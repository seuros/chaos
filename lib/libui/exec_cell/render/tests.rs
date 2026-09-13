use super::*;
use chaos_ipc::protocol::ExecCommandSource;
use pretty_assertions::assert_eq;

fn user_shell_output_is_limited_by_screen_lines() {
    let long_url_like = format!(
        "https://example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/{}",
        "very-long-segment-".repeat(120),
    );
    let aggregated_output = format!("{long_url_like}\n{long_url_like}\n");

    // Baseline: how many screen lines would we get if we simply wrapped
    // all logical lines without any truncation?
    let output = CommandOutput {
        exit_code: 0,
        aggregated_output,
        formatted_output: String::new(),
    };
    let width = 20;
    let layout = EXEC_DISPLAY_LAYOUT;
    let raw_output = output_lines(
        Some(&output),
        OutputLinesParams {
            // Large enough to include all logical lines without
            // triggering the ellipsis in `output_lines`.
            line_limit: 100,
            only_err: false,
            include_angle_pipe: false,
            include_prefix: false,
        },
    );
    let output_wrap_width = layout.output_block.wrap_width(width);
    let output_opts = RtOptions::new(output_wrap_width).word_splitter(WordSplitter::NoHyphenation);
    let mut full_wrapped_output: Vec<Line<'static>> = Vec::new();
    for line in &raw_output.lines {
        push_owned_lines(
            &adaptive_wrap_line(line, output_opts.clone()),
            &mut full_wrapped_output,
        );
    }
    let full_prefixed_output = prefix_lines(
        full_wrapped_output,
        Span::from(layout.output_block.initial_prefix).dim(),
        Span::from(layout.output_block.subsequent_prefix),
    );
    let full_screen_lines = Paragraph::new(Text::from(full_prefixed_output))
        .wrap(Wrap { trim: false })
        .line_count(width);

    // Sanity check: this scenario should produce more screen lines than
    // the user shell per-call limit when no truncation is applied. If
    // this ever fails, the test no longer exercises the regression.
    assert!(
        full_screen_lines > USER_SHELL_TOOL_CALL_MAX_LINES,
        "expected unbounded wrapping to produce more than {USER_SHELL_TOOL_CALL_MAX_LINES} screen lines, got {full_screen_lines}",
    );

    let call = ExecCall {
        call_id: "call-id".to_string(),
        command: vec!["bash".into(), "-lc".into(), "echo long".into()],
        parsed: Vec::new(),
        output: Some(output),
        source: ExecCommandSource::UserShell,
        start_time: None,
        duration: None,
        interaction_input: None,
    };

    let cell = ExecCell::new(call, false);

    // Use a narrow width so each logical line wraps into many on-screen lines.
    let lines = cell.command_display_lines(width);
    let rendered_rows = Paragraph::new(Text::from(lines.clone()))
        .wrap(Wrap { trim: false })
        .line_count(width);
    let header_rows = Paragraph::new(Text::from(vec![lines[0].clone()]))
        .wrap(Wrap { trim: false })
        .line_count(width);
    let output_screen_rows = rendered_rows.saturating_sub(header_rows);

    let contains_ellipsis = lines
        .iter()
        .any(|line| line.spans.iter().any(|span| span.content.contains("… +")));

    // Regression guard: previously this scenario could render hundreds of
    // wrapped rows because truncation happened before final viewport
    // wrapping. The row-aware truncation now caps visible output rows.
    assert!(
        output_screen_rows <= USER_SHELL_TOOL_CALL_MAX_LINES,
        "expected at most {USER_SHELL_TOOL_CALL_MAX_LINES} output rows, got {output_screen_rows} (total rows: {rendered_rows})",
    );
    assert!(
        contains_ellipsis,
        "expected truncated output to include an ellipsis line"
    );
}

pub(crate) fn exec_cell_render_suite() {
    user_shell_output_is_limited_by_screen_lines();
    truncate_lines_middle_keeps_omitted_count_in_line_units();
    truncate_lines_middle_does_not_truncate_blank_prefixed_output_lines();
    command_display_does_not_split_long_url_token();
    exploring_display_does_not_split_long_url_like_search_query();
    output_display_does_not_split_long_url_like_token_without_scheme();
    desired_transcript_height_accounts_for_wrapped_url_like_rows();
}
#[cfg(test)]
fn truncate_lines_middle_keeps_omitted_count_in_line_units() {
    let lines = vec![
        Line::from("  └ short"),
        Line::from("    this-is-a-very-long-token-that-wraps-many-rows"),
        Line::from("    … +4 lines"),
        Line::from("    tail"),
    ];

    let truncated =
        ExecCell::truncate_lines_middle(&lines, 2, 12, Some(4), Some(Line::from("    ".dim())));
    let rendered: Vec<String> = truncated
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();

    assert!(
        rendered.iter().any(|line| line.contains("… +6 lines")),
        "expected omitted hint to count hidden lines (not wrapped rows), got: {rendered:?}"
    );
}

#[cfg(test)]
fn truncate_lines_middle_does_not_truncate_blank_prefixed_output_lines() {
    let mut lines = vec![Line::from("  └ start")];
    lines.extend(std::iter::repeat_n(Line::from("    "), 26));
    lines.push(Line::from("    end"));

    let truncated = ExecCell::truncate_lines_middle(&lines, 28, 80, None, None);

    assert_eq!(truncated, lines);
}

#[cfg(test)]
fn command_display_does_not_split_long_url_token() {
    let url = "http://example.com/long-url-with-dashes-wider-than-terminal-window/blah-blah-blah-text/more-gibberish-text";

    let call = ExecCall {
        call_id: "call-id".to_string(),
        command: vec!["bash".into(), "-lc".into(), format!("echo {url}")],
        parsed: Vec::new(),
        output: None,
        source: ExecCommandSource::UserShell,
        start_time: None,
        duration: None,
        interaction_input: None,
    };

    let cell = ExecCell::new(call, false);
    let rendered: Vec<String> = cell
        .command_display_lines(36)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();

    assert_eq!(
        rendered.iter().filter(|line| line.contains(url)).count(),
        1,
        "expected full URL in one rendered line, got: {rendered:?}"
    );
}

#[cfg(test)]
fn exploring_display_does_not_split_long_url_like_search_query() {
    let url_like = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/with/a/very/long/path";
    let call = ExecCall {
        call_id: "call-id".to_string(),
        command: vec!["bash".into(), "-lc".into(), "rg foo".into()],
        parsed: vec![ParsedCommand::Search {
            cmd: format!("rg {url_like}"),
            query: Some(url_like.to_string()),
            path: None,
        }],
        output: None,
        source: ExecCommandSource::Agent,
        start_time: None,
        duration: None,
        interaction_input: None,
    };

    let cell = ExecCell::new(call, false);
    let rendered: Vec<String> = cell
        .display_lines(36)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();

    assert_eq!(
        rendered
            .iter()
            .filter(|line| line.contains(url_like))
            .count(),
        1,
        "expected full URL-like query in one rendered line, got: {rendered:?}"
    );
}

#[cfg(test)]
fn output_display_does_not_split_long_url_like_token_without_scheme() {
    let url = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/session_id=abc123def456ghi789jkl012mno345pqr678";

    let call = ExecCall {
        call_id: "call-id".to_string(),
        command: vec!["bash".into(), "-lc".into(), "echo done".into()],
        parsed: Vec::new(),
        output: Some(CommandOutput {
            exit_code: 0,
            formatted_output: String::new(),
            aggregated_output: url.to_string(),
        }),
        source: ExecCommandSource::UserShell,
        start_time: None,
        duration: None,
        interaction_input: None,
    };

    let cell = ExecCell::new(call, false);
    let rendered: Vec<String> = cell
        .command_display_lines(36)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();

    assert_eq!(
        rendered.iter().filter(|line| line.contains(url)).count(),
        1,
        "expected full URL-like token in one rendered line, got: {rendered:?}"
    );
}

#[cfg(test)]
fn desired_transcript_height_accounts_for_wrapped_url_like_rows() {
    let url = "https://example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/with/a/very/long/path/that/keeps/going/for/testing/purposes";
    let call = ExecCall {
        call_id: "call-id".to_string(),
        command: vec!["bash".into(), "-lc".into(), "echo done".into()],
        parsed: Vec::new(),
        output: Some(CommandOutput {
            exit_code: 0,
            formatted_output: url.to_string(),
            aggregated_output: url.to_string(),
        }),
        source: ExecCommandSource::Agent,
        start_time: None,
        duration: None,
        interaction_input: None,
    };

    let cell = ExecCell::new(call, false);
    let width: u16 = 36;
    let logical_height = cell.transcript_lines(width).len() as u16;
    let wrapped_height = cell.desired_transcript_height(width);

    assert!(
        wrapped_height > logical_height,
        "expected transcript height to account for wrapped URL-like rows, logical_height={logical_height}, wrapped_height={wrapped_height}"
    );
}
