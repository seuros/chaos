use crate::markdown::append_markdown;
use crate::render::line_utils::prefix_lines;
use crate::render::line_utils::push_owned_lines;
use crate::ui_consts::LIVE_PREFIX_COLS;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use crate::wrapping::adaptive_wrap_lines;
use chaos_ipc::user_input::TextElement;
use ratatui::prelude::*;
use ratatui::style::Stylize;
use std::path::Path;
use std::path::PathBuf;

use super::super::render;
use super::trait_def::HistoryCell;

// ---------------------------------------------------------------------------
// UserHistoryCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct UserHistoryCell {
    pub message: String,
    pub text_elements: Vec<TextElement>,
    #[allow(dead_code)]
    pub local_image_paths: Vec<PathBuf>,
    pub remote_image_urls: Vec<String>,
}

impl HistoryCell for UserHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let wrap_width = width.saturating_sub(LIVE_PREFIX_COLS + 1).max(1);

        let style = crate::style::user_message_style();
        let element_style = style.fg(crate::theme::cyan());

        let wrapped_remote_images = if self.remote_image_urls.is_empty() {
            None
        } else {
            Some(adaptive_wrap_lines(
                self.remote_image_urls
                    .iter()
                    .enumerate()
                    .map(|(idx, _url)| {
                        render::remote_image_display_line(element_style, idx.saturating_add(1))
                    }),
                RtOptions::new(usize::from(wrap_width))
                    .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
            ))
        };

        let wrapped_message = if self.message.is_empty() && self.text_elements.is_empty() {
            None
        } else if self.text_elements.is_empty() {
            let message_without_trailing_newlines = self.message.trim_end_matches(['\r', '\n']);
            let wrapped = adaptive_wrap_lines(
                message_without_trailing_newlines
                    .split('\n')
                    .map(|line| Line::from(line).style(style)),
                RtOptions::new(usize::from(wrap_width))
                    .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
            );
            let wrapped = render::trim_trailing_blank_lines(wrapped);
            (!wrapped.is_empty()).then_some(wrapped)
        } else {
            let raw_lines = render::build_user_message_lines_with_elements(
                &self.message,
                &self.text_elements,
                style,
                element_style,
            );
            let wrapped = adaptive_wrap_lines(
                raw_lines,
                RtOptions::new(usize::from(wrap_width))
                    .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
            );
            let wrapped = render::trim_trailing_blank_lines(wrapped);
            (!wrapped.is_empty()).then_some(wrapped)
        };

        if wrapped_remote_images.is_none() && wrapped_message.is_none() {
            return Vec::new();
        }

        let mut lines: Vec<Line<'static>> = vec![Line::from("").style(style)];

        if let Some(wrapped_remote_images) = wrapped_remote_images {
            lines.extend(prefix_lines(
                wrapped_remote_images,
                "  ".into(),
                "  ".into(),
            ));
            if wrapped_message.is_some() {
                lines.push(Line::from("").style(style));
            }
        }

        if let Some(wrapped_message) = wrapped_message {
            lines.extend(prefix_lines(
                wrapped_message,
                "› ".bold().dim(),
                "  ".into(),
            ));
        }

        lines.push(Line::from("").style(style));
        lines
    }
}

pub fn new_user_prompt(
    message: String,
    text_elements: Vec<TextElement>,
    local_image_paths: Vec<PathBuf>,
    remote_image_urls: Vec<String>,
) -> UserHistoryCell {
    UserHistoryCell {
        message,
        text_elements,
        local_image_paths,
        remote_image_urls,
    }
}

// ---------------------------------------------------------------------------
// ReasoningSummaryCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ReasoningSummaryCell {
    _header: String,
    content: String,
    /// Session cwd used to render local file links inside the reasoning body.
    cwd: PathBuf,
    transcript_only: bool,
}

impl ReasoningSummaryCell {
    pub fn new(header: String, content: String, cwd: &Path, transcript_only: bool) -> Self {
        Self {
            _header: header,
            content,
            cwd: cwd.to_path_buf(),
            transcript_only,
        }
    }

    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        append_markdown(
            &self.content,
            Some((width as usize).saturating_sub(2)),
            Some(self.cwd.as_path()),
            &mut lines,
        );
        let summary_style = ratatui::style::Style::default().dim().italic();
        let summary_lines = lines
            .into_iter()
            .map(|mut line| {
                line.spans = line
                    .spans
                    .into_iter()
                    .map(|span| span.patch_style(summary_style))
                    .collect();
                line
            })
            .collect::<Vec<_>>();

        adaptive_wrap_lines(
            &summary_lines,
            RtOptions::new(width as usize)
                .initial_indent("• ".dim().into())
                .subsequent_indent("  ".into()),
        )
    }
}

impl HistoryCell for ReasoningSummaryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.transcript_only {
            Vec::new()
        } else {
            self.lines(width)
        }
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.lines(width)
    }
}

/// Create the reasoning history cell emitted at the end of a reasoning block.
pub fn new_reasoning_summary_block(
    full_reasoning_buffer: String,
    cwd: &Path,
) -> Box<dyn HistoryCell> {
    let cwd = cwd.to_path_buf();
    let full_reasoning_buffer = full_reasoning_buffer.trim();
    if let Some(open) = full_reasoning_buffer.find("**") {
        let after_open = &full_reasoning_buffer[(open + 2)..];
        if let Some(close) = after_open.find("**") {
            let after_close_idx = open + 2 + close + 2;
            if after_close_idx < full_reasoning_buffer.len() {
                let header_buffer = full_reasoning_buffer[..after_close_idx].to_string();
                let summary_buffer = full_reasoning_buffer[after_close_idx..].to_string();
                return Box::new(ReasoningSummaryCell::new(
                    header_buffer,
                    summary_buffer,
                    &cwd,
                    false,
                ));
            }
        }
    }
    Box::new(ReasoningSummaryCell::new(
        "".to_string(),
        full_reasoning_buffer.to_string(),
        &cwd,
        true,
    ))
}

// ---------------------------------------------------------------------------
// AgentMessageCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AgentMessageCell {
    lines: Vec<Line<'static>>,
    is_first_line: bool,
}

impl AgentMessageCell {
    pub fn new(lines: Vec<Line<'static>>, is_first_line: bool) -> Self {
        Self {
            lines,
            is_first_line,
        }
    }
}

impl HistoryCell for AgentMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        adaptive_wrap_lines(
            &self.lines,
            RtOptions::new(width as usize)
                .initial_indent(if self.is_first_line {
                    "• ".dim().into()
                } else {
                    "  ".into()
                })
                .subsequent_indent("  ".into()),
        )
    }

    fn is_stream_continuation(&self) -> bool {
        !self.is_first_line
    }
}

// ---------------------------------------------------------------------------
// PlainHistoryCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PlainHistoryCell {
    pub(super) lines: Vec<Line<'static>>,
}

impl PlainHistoryCell {
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines }
    }
}

impl HistoryCell for PlainHistoryCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }
}

// ---------------------------------------------------------------------------
// PrefixedWrappedHistoryCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PrefixedWrappedHistoryCell {
    text: Text<'static>,
    initial_prefix: Line<'static>,
    subsequent_prefix: Line<'static>,
}

impl PrefixedWrappedHistoryCell {
    pub fn new(
        text: impl Into<Text<'static>>,
        initial_prefix: impl Into<Line<'static>>,
        subsequent_prefix: impl Into<Line<'static>>,
    ) -> Self {
        Self {
            text: text.into(),
            initial_prefix: initial_prefix.into(),
            subsequent_prefix: subsequent_prefix.into(),
        }
    }
}

impl HistoryCell for PrefixedWrappedHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let opts = RtOptions::new(width.max(1) as usize)
            .initial_indent(self.initial_prefix.clone())
            .subsequent_indent(self.subsequent_prefix.clone());
        adaptive_wrap_lines(&self.text, opts)
    }
}

// ---------------------------------------------------------------------------
// UnifiedExecInteractionCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct UnifiedExecInteractionCell {
    command_display: Option<String>,
    stdin: String,
}

impl UnifiedExecInteractionCell {
    pub fn new(command_display: Option<String>, stdin: String) -> Self {
        Self {
            command_display,
            stdin,
        }
    }
}

impl HistoryCell for UnifiedExecInteractionCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let wrap_width = width as usize;
        let waited_only = self.stdin.is_empty();

        let mut header_spans = if waited_only {
            vec!["• Waited for background terminal".bold()]
        } else {
            vec!["↳ ".dim(), "Interacted with background terminal".bold()]
        };
        if let Some(command) = &self.command_display
            && !command.is_empty()
        {
            header_spans.push(" · ".dim());
            header_spans.push(command.clone().dim());
        }
        let header = Line::from(header_spans);

        let mut out: Vec<Line<'static>> = Vec::new();
        let header_wrapped = adaptive_wrap_line(&header, RtOptions::new(wrap_width));
        push_owned_lines(&header_wrapped, &mut out);

        if waited_only {
            return out;
        }

        let input_lines: Vec<Line<'static>> = self
            .stdin
            .lines()
            .map(|line| Line::from(line.to_string()))
            .collect();

        let input_wrapped = adaptive_wrap_lines(
            input_lines,
            RtOptions::new(wrap_width)
                .initial_indent(Line::from("  └ ".dim()))
                .subsequent_indent(Line::from("    ".dim())),
        );
        out.extend(input_wrapped);
        out
    }
}

pub fn new_unified_exec_interaction(
    command_display: Option<String>,
    stdin: String,
) -> UnifiedExecInteractionCell {
    UnifiedExecInteractionCell::new(command_display, stdin)
}
