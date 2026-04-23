//! Pure rendering helpers for history cells.
//!
//! This module contains stateless utility functions used by the various cell
//! implementations: border drawing, line-wrapping helpers, MCP invocation
//! formatting, runtime metrics labels, and image decoding.

use crate::render::line_utils::push_owned_lines;
use crate::render::renderable::Renderable;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use base64::Engine;
use chaos_ipc::models::local_image_label_text;
use chaos_ipc::protocol::McpInvocation;
use chaos_ipc::request_user_input::RequestUserInputAnswer;
use chaos_syslog::RuntimeMetricsSummary;
use image::DynamicImage;
use image::ImageReader;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::io::Cursor;
use tracing::error;
use unicode_width::UnicodeWidthStr;

use super::state::HistoryCell;

// ---------------------------------------------------------------------------
// Renderable impl for boxed trait objects
// ---------------------------------------------------------------------------

impl Renderable for Box<dyn HistoryCell> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.display_lines(area.width);
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let y = if area.height == 0 {
            0
        } else {
            let overflow = paragraph
                .line_count(area.width)
                .saturating_sub(usize::from(area.height));
            u16::try_from(overflow).unwrap_or(u16::MAX)
        };
        paragraph.scroll((y, 0)).render(area, buf);
    }
    fn desired_height(&self, width: u16) -> u16 {
        HistoryCell::desired_height(self.as_ref(), width)
    }
}

// ---------------------------------------------------------------------------
// User message helpers
// ---------------------------------------------------------------------------

/// Build logical lines for a user message with styled text elements.
///
/// This preserves explicit newlines while interleaving element spans and skips
/// malformed byte ranges instead of panicking during history rendering.
pub(super) fn build_user_message_lines_with_elements(
    message: &str,
    elements: &[chaos_ipc::user_input::TextElement],
    style: Style,
    element_style: Style,
) -> Vec<Line<'static>> {
    let mut elements = elements.to_vec();
    elements.sort_by_key(|e| e.byte_range.start);
    let mut offset = 0usize;
    let mut raw_lines: Vec<Line<'static>> = Vec::new();
    for line_text in message.split('\n') {
        let line_start = offset;
        let line_end = line_start + line_text.len();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut cursor = line_start;
        for elem in &elements {
            let start = elem.byte_range.start.max(line_start);
            let end = elem.byte_range.end.min(line_end);
            if start >= end {
                continue;
            }
            let rel_start = start - line_start;
            let rel_end = end - line_start;
            if !line_text.is_char_boundary(rel_start) || !line_text.is_char_boundary(rel_end) {
                continue;
            }
            let rel_cursor = cursor - line_start;
            if cursor < start
                && line_text.is_char_boundary(rel_cursor)
                && let Some(segment) = line_text.get(rel_cursor..rel_start)
            {
                spans.push(Span::from(segment.to_string()));
            }
            if let Some(segment) = line_text.get(rel_start..rel_end) {
                spans.push(Span::styled(segment.to_string(), element_style));
                cursor = end;
            }
        }
        let rel_cursor = cursor - line_start;
        if cursor < line_end
            && line_text.is_char_boundary(rel_cursor)
            && let Some(segment) = line_text.get(rel_cursor..)
        {
            spans.push(Span::from(segment.to_string()));
        }
        let line = if spans.is_empty() {
            Line::from(line_text.to_string()).style(style)
        } else {
            Line::from(spans).style(style)
        };
        raw_lines.push(line);
        offset = line_end + 1;
    }

    raw_lines
}

pub(super) fn remote_image_display_line(style: Style, index: usize) -> Line<'static> {
    Line::from(local_image_label_text(index)).style(style)
}

pub(super) fn trim_trailing_blank_lines(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    while lines
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.trim().is_empty()))
    {
        lines.pop();
    }
    lines
}

// ---------------------------------------------------------------------------
// Border rendering
// ---------------------------------------------------------------------------

/// Render `lines` inside a border whose inner width is at least `inner_width`.
pub fn with_border_with_inner_width(
    lines: Vec<Line<'static>>,
    inner_width: usize,
) -> Vec<Line<'static>> {
    with_border_internal(lines, Some(inner_width))
}

pub(super) fn with_border_internal(
    lines: Vec<Line<'static>>,
    forced_inner_width: Option<usize>,
) -> Vec<Line<'static>> {
    let max_line_width = lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    let content_width = forced_inner_width
        .unwrap_or(max_line_width)
        .max(max_line_width);

    let mut out = Vec::with_capacity(lines.len() + 2);
    let border_inner_width = content_width + 2;
    let border_style = crate::theme::border();
    out.push(
        vec![Span::styled(
            format!("╭{}╮", "─".repeat(border_inner_width)),
            border_style,
        )]
        .into(),
    );

    for line in lines.into_iter() {
        let used_width: usize = line
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        let span_count = line.spans.len();
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(span_count + 4);
        spans.push(Span::styled("│ ", border_style));
        spans.extend(line);
        if used_width < content_width {
            spans.push(Span::from(" ".repeat(content_width - used_width)));
        }
        spans.push(Span::styled(" │", border_style));
        out.push(Line::from(spans));
    }

    out.push(
        vec![Span::styled(
            format!("╰{}╯", "─".repeat(border_inner_width)),
            border_style,
        )]
        .into(),
    );

    out
}

// ---------------------------------------------------------------------------
// MCP invocation formatting
// ---------------------------------------------------------------------------

pub(super) fn format_mcp_invocation(invocation: McpInvocation) -> Line<'static> {
    let args_str = invocation
        .arguments
        .as_ref()
        .map(|v: &serde_json::Value| serde_json::to_string(v).unwrap_or_else(|_| v.to_string()))
        .unwrap_or_default();

    let invocation_spans = vec![
        invocation.server.clone().cyan(),
        ".".into(),
        invocation.tool.cyan(),
        "(".into(),
        args_str.dim(),
        ")".into(),
    ];
    invocation_spans.into()
}

// ---------------------------------------------------------------------------
// MCP image decoding
// ---------------------------------------------------------------------------

/// Returns an additional history cell if an MCP tool result includes a decodable image.
pub(super) fn try_new_completed_mcp_tool_call_with_image_output(
    result: &Result<chaos_ipc::mcp::CallToolResult, String>,
) -> Option<super::state::CompletedMcpToolCallWithImageOutput> {
    let image = result
        .as_ref()
        .ok()?
        .content
        .iter()
        .find_map(decode_mcp_image)?;

    Some(super::state::CompletedMcpToolCallWithImageOutput { _image: image })
}

/// Decodes an MCP `ImageContent` block into an in-memory image.
pub(super) fn decode_mcp_image(block: &serde_json::Value) -> Option<DynamicImage> {
    let content = serde_json::from_value::<mcp_guest::ContentBlock>(block.clone()).ok()?;
    let mcp_guest::ContentBlock::Image {
        data, mime_type: _, ..
    } = content
    else {
        return None;
    };
    let base64_data = if let Some(data_url) = data.strip_prefix("data:") {
        data_url.split_once(',')?.1
    } else {
        data.as_str()
    };
    let raw_data = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .map_err(|e| {
            error!("Failed to decode image data: {e}");
            e
        })
        .ok()?;
    let reader = ImageReader::new(Cursor::new(raw_data))
        .with_guessed_format()
        .map_err(|e| {
            error!("Failed to guess image format: {e}");
            e
        })
        .ok()?;

    reader
        .decode()
        .map_err(|e| {
            error!("Image decoding failed: {e}");
            e
        })
        .ok()
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

pub(super) fn web_search_header(completed: bool) -> &'static str {
    if completed {
        "Searched"
    } else {
        "Searching the web"
    }
}

pub(super) fn truncate_exec_snippet(full_cmd: &str) -> String {
    let mut snippet = match full_cmd.split_once('\n') {
        Some((first, _)) => format!("{first} ..."),
        None => full_cmd.to_string(),
    };
    snippet = crate::text_formatting::truncate_text(&snippet, 80);
    snippet
}

pub(super) fn exec_snippet(command: &[String]) -> String {
    let full_cmd = crate::exec_command::strip_bash_lc_and_escape(command);
    truncate_exec_snippet(&full_cmd)
}

/// Wrap a plain string with textwrap and prefix each line, applying a style to the content.
pub(super) fn wrap_with_prefix(
    text: &str,
    width: usize,
    initial_prefix: Span<'static>,
    subsequent_prefix: Span<'static>,
    style: Style,
) -> Vec<Line<'static>> {
    let line = Line::from(vec![Span::styled(text.to_string(), style)]);
    let opts = RtOptions::new(width.max(1))
        .initial_indent(Line::from(vec![initial_prefix]))
        .subsequent_indent(Line::from(vec![subsequent_prefix]));
    let wrapped = adaptive_wrap_line(&line, opts);
    let mut out = Vec::new();
    push_owned_lines(&wrapped, &mut out);
    out
}

/// Split a request_user_input answer into option labels and an optional freeform note.
pub(super) fn split_request_user_input_answer(
    answer: &RequestUserInputAnswer,
) -> (Vec<String>, Option<String>) {
    let mut options = Vec::new();
    let mut note = None;
    for entry in &answer.answers {
        if let Some(note_text) = entry.strip_prefix("user_note: ") {
            note = Some(note_text.to_string());
        } else {
            options.push(entry.clone());
        }
    }
    (options, note)
}

// ---------------------------------------------------------------------------
// Runtime metrics helpers
// ---------------------------------------------------------------------------

pub fn runtime_metrics_label(summary: RuntimeMetricsSummary) -> Option<String> {
    let mut parts = Vec::new();
    if summary.tool_calls.count > 0 {
        let duration = format_duration_ms(summary.tool_calls.duration_ms);
        let calls = pluralize(summary.tool_calls.count, "call", "calls");
        parts.push(format!(
            "Local tools: {} {calls} ({duration})",
            summary.tool_calls.count
        ));
    }
    if summary.api_calls.count > 0 {
        let duration = format_duration_ms(summary.api_calls.duration_ms);
        let calls = pluralize(summary.api_calls.count, "call", "calls");
        parts.push(format!(
            "Inference: {} {calls} ({duration})",
            summary.api_calls.count
        ));
    }
    if summary.streaming_events.count > 0 {
        let duration = format_duration_ms(summary.streaming_events.duration_ms);
        let stream_label = pluralize(summary.streaming_events.count, "Stream", "Streams");
        let events = pluralize(summary.streaming_events.count, "event", "events");
        parts.push(format!(
            "{stream_label}: {} {events} ({duration})",
            summary.streaming_events.count
        ));
    }
    if summary.responses_api_overhead_ms > 0 {
        let duration = format_duration_ms(summary.responses_api_overhead_ms);
        parts.push(format!("Responses API overhead: {duration}"));
    }
    if summary.responses_api_inference_time_ms > 0 {
        let duration = format_duration_ms(summary.responses_api_inference_time_ms);
        parts.push(format!("Responses API inference: {duration}"));
    }
    if summary.responses_api_engine_iapi_ttft_ms > 0
        || summary.responses_api_engine_service_ttft_ms > 0
    {
        let mut ttft_parts = Vec::new();
        if summary.responses_api_engine_iapi_ttft_ms > 0 {
            let duration = format_duration_ms(summary.responses_api_engine_iapi_ttft_ms);
            ttft_parts.push(format!("{duration} (iapi)"));
        }
        if summary.responses_api_engine_service_ttft_ms > 0 {
            let duration = format_duration_ms(summary.responses_api_engine_service_ttft_ms);
            ttft_parts.push(format!("{duration} (service)"));
        }
        parts.push(format!("TTFT: {}", ttft_parts.join(" ")));
    }
    if summary.responses_api_engine_iapi_tbt_ms > 0
        || summary.responses_api_engine_service_tbt_ms > 0
    {
        let mut tbt_parts = Vec::new();
        if summary.responses_api_engine_iapi_tbt_ms > 0 {
            let duration = format_duration_ms(summary.responses_api_engine_iapi_tbt_ms);
            tbt_parts.push(format!("{duration} (iapi)"));
        }
        if summary.responses_api_engine_service_tbt_ms > 0 {
            let duration = format_duration_ms(summary.responses_api_engine_service_tbt_ms);
            tbt_parts.push(format!("{duration} (service)"));
        }
        parts.push(format!("TBT: {}", tbt_parts.join(" ")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" • "))
    }
}

pub(super) fn format_duration_ms(duration_ms: u64) -> String {
    if duration_ms >= 1_000 {
        let seconds = duration_ms as f64 / 1_000.0;
        format!("{seconds:.1}s")
    } else {
        format!("{duration_ms}ms")
    }
}

pub(super) fn pluralize(count: u64, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}
