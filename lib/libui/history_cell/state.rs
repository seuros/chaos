//! State structs and models for history cells.
//!
//! Contains the `HistoryCell` trait and every concrete cell type together with
//! their `HistoryCell` implementations and constructor helpers.

use crate::exec_cell::TOOL_CALL_MAX_LINES;
use crate::exec_cell::spinner;
use crate::live_wrap::take_prefix_by_width;
use crate::markdown::append_markdown;
use crate::render::line_utils::line_to_static;
use crate::render::line_utils::prefix_lines;
use crate::render::line_utils::push_owned_lines;
use crate::style::proposed_plan_style;
use crate::style::user_message_style;
use crate::text_formatting::format_and_truncate_tool_result;
use crate::ui_consts::LIVE_PREFIX_COLS;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use crate::wrapping::adaptive_wrap_lines;
use chaos_getopt::format_env_display::format_env_display;
use chaos_ipc::mcp::Resource;
use chaos_ipc::mcp::ResourceTemplate;
use chaos_ipc::models::WebSearchAction;
use chaos_ipc::plan_tool::PlanItemArg;
use chaos_ipc::plan_tool::StepStatus;
use chaos_ipc::plan_tool::UpdatePlanArgs;
use chaos_ipc::protocol::McpAuthStatus;
use chaos_ipc::protocol::McpInvocation;
use chaos_ipc::protocol::SessionConfiguredEvent;
use chaos_ipc::request_user_input::RequestUserInputAnswer;
use chaos_ipc::request_user_input::RequestUserInputQuestion;
use chaos_ipc::user_input::TextElement;
use chaos_kern::config::Config;
use chaos_kern::config::types::McpServerTransportConfig;
use chaos_kern::mcp::McpManager;
use chaos_kern::web_search::web_search_detail;
use chaos_syslog::RuntimeMetricsSummary;
use image::DynamicImage;
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Styled;
use ratatui::style::Stylize;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::any::Any;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::render;

// ---------------------------------------------------------------------------
// Core trait
// ---------------------------------------------------------------------------

/// A single renderable unit of conversation history.
///
/// Each cell produces logical `Line`s and reports how many viewport rows those
/// lines occupy at a given terminal width. The default height implementations
/// use `Paragraph::wrap` to account for lines that overflow the viewport width
/// (e.g. long URLs that are kept intact by adaptive wrapping). Concrete types
/// only need to override heights when they apply additional layout logic beyond
/// what `Paragraph::line_count` captures.
pub trait HistoryCell: std::fmt::Debug + Send + Sync + Any {
    /// Returns the logical lines for the main chat viewport.
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;

    /// Returns the number of viewport rows needed to render this cell.
    fn desired_height(&self, width: u16) -> u16 {
        Paragraph::new(Text::from(self.display_lines(width)))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }

    /// Returns lines for the transcript overlay (`Ctrl+T`).
    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.display_lines(width)
    }

    /// Returns the number of viewport rows for the transcript overlay.
    fn desired_transcript_height(&self, width: u16) -> u16 {
        let lines = self.transcript_lines(width);
        // Workaround: ratatui's line_count returns 2 for a single
        // whitespace-only line. Clamp to 1 in that case.
        if let [line] = &lines[..]
            && line
                .spans
                .iter()
                .all(|s| s.content.chars().all(char::is_whitespace))
        {
            return 1;
        }

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }

    fn is_stream_continuation(&self) -> bool {
        false
    }

    /// Returns a coarse "animation tick" when transcript output is time-dependent.
    fn transcript_animation_tick(&self) -> Option<u64> {
        None
    }
}

impl dyn HistoryCell {
    pub fn as_any(&self) -> &dyn Any {
        self
    }

    pub fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

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

        let style = user_message_style();
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
        let summary_style = Style::default().dim().italic();
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

// ---------------------------------------------------------------------------
// UnifiedExecProcessesCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct UnifiedExecProcessesCell {
    processes: Vec<UnifiedExecProcessDetails>,
}

impl UnifiedExecProcessesCell {
    fn new(processes: Vec<UnifiedExecProcessDetails>) -> Self {
        Self { processes }
    }
}

#[derive(Debug, Clone)]
pub struct UnifiedExecProcessDetails {
    pub command_display: String,
    pub recent_chunks: Vec<String>,
}

impl HistoryCell for UnifiedExecProcessesCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }

        let wrap_width = width as usize;
        let max_processes = 16usize;
        let mut out: Vec<Line<'static>> = Vec::new();
        out.push(vec!["Background terminals".bold()].into());
        out.push("".into());

        if self.processes.is_empty() {
            out.push("  • No background terminals running.".italic().into());
            return out;
        }

        let prefix = "  • ";
        let prefix_width = UnicodeWidthStr::width(prefix);
        let truncation_suffix = " [...]";
        let truncation_suffix_width = UnicodeWidthStr::width(truncation_suffix);
        let mut shown = 0usize;
        for process in &self.processes {
            if shown >= max_processes {
                break;
            }
            let command = &process.command_display;
            let (snippet, snippet_truncated) = {
                let (first_line, has_more_lines) = match command.split_once('\n') {
                    Some((first, _)) => (first, true),
                    None => (command.as_str(), false),
                };
                let max_graphemes = 80;
                let mut graphemes = first_line.grapheme_indices(true);
                if let Some((byte_index, _)) = graphemes.nth(max_graphemes) {
                    (first_line[..byte_index].to_string(), true)
                } else {
                    (first_line.to_string(), has_more_lines)
                }
            };
            if wrap_width <= prefix_width {
                out.push(Line::from(prefix.dim()));
                shown += 1;
                continue;
            }
            let budget = wrap_width.saturating_sub(prefix_width);
            let mut needs_suffix = snippet_truncated;
            if !needs_suffix {
                let (_, remainder, _) = take_prefix_by_width(&snippet, budget);
                if !remainder.is_empty() {
                    needs_suffix = true;
                }
            }
            if needs_suffix && budget > truncation_suffix_width {
                let available = budget.saturating_sub(truncation_suffix_width);
                let (truncated, _, _) = take_prefix_by_width(&snippet, available);
                out.push(vec![prefix.dim(), truncated.cyan(), truncation_suffix.dim()].into());
            } else {
                let (truncated, _, _) = take_prefix_by_width(&snippet, budget);
                out.push(vec![prefix.dim(), truncated.cyan()].into());
            }

            let chunk_prefix_first = "    ↳ ";
            let chunk_prefix_next = "      ";
            for (idx, chunk) in process.recent_chunks.iter().enumerate() {
                let chunk_prefix = if idx == 0 {
                    chunk_prefix_first
                } else {
                    chunk_prefix_next
                };
                let chunk_prefix_width = UnicodeWidthStr::width(chunk_prefix);
                if wrap_width <= chunk_prefix_width {
                    out.push(Line::from(chunk_prefix.dim()));
                    continue;
                }
                let budget = wrap_width.saturating_sub(chunk_prefix_width);
                let (truncated, remainder, _) = take_prefix_by_width(chunk, budget);
                if !remainder.is_empty() && budget > truncation_suffix_width {
                    let available = budget.saturating_sub(truncation_suffix_width);
                    let (shorter, _, _) = take_prefix_by_width(chunk, available);
                    out.push(
                        vec![chunk_prefix.dim(), shorter.dim(), truncation_suffix.dim()].into(),
                    );
                } else {
                    out.push(vec![chunk_prefix.dim(), truncated.dim()].into());
                }
            }
            shown += 1;
        }

        let remaining = self.processes.len().saturating_sub(shown);
        if remaining > 0 {
            let more_text = format!("... and {remaining} more running");
            if wrap_width <= prefix_width {
                out.push(Line::from(prefix.dim()));
            } else {
                let budget = wrap_width.saturating_sub(prefix_width);
                let (truncated, _, _) = take_prefix_by_width(&more_text, budget);
                out.push(vec![prefix.dim(), truncated.dim()].into());
            }
        }

        out
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.display_lines(width).len() as u16
    }
}

pub fn new_unified_exec_processes_output(
    processes: Vec<UnifiedExecProcessDetails>,
) -> CompositeHistoryCell {
    let command = PlainHistoryCell::new(vec!["/ps".magenta().into()]);
    let summary = UnifiedExecProcessesCell::new(processes);
    CompositeHistoryCell::new(vec![Box::new(command), Box::new(summary)])
}

// ---------------------------------------------------------------------------
// ApprovalDecisionActor + new_approval_decision_cell
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecisionActor {
    User,
}

impl ApprovalDecisionActor {
    fn subject(self) -> &'static str {
        match self {
            Self::User => "You ",
        }
    }
}

pub fn new_approval_decision_cell(
    command: Vec<String>,
    decision: chaos_ipc::protocol::ReviewDecision,
    actor: ApprovalDecisionActor,
) -> Box<dyn HistoryCell> {
    use chaos_ipc::protocol::NetworkPolicyRuleAction;
    use chaos_ipc::protocol::ReviewDecision::*;

    let (symbol, summary): (Span<'static>, Vec<Span<'static>>) = match decision {
        Approved => {
            let snippet = Span::from(render::exec_snippet(&command)).dim();
            (
                "✔ ".green(),
                vec![
                    actor.subject().into(),
                    "approved".bold(),
                    " chaos to run ".into(),
                    snippet,
                    " this time".bold(),
                ],
            )
        }
        ApprovedExecpolicyAmendment {
            proposed_execpolicy_amendment,
        } => {
            let snippet =
                Span::from(render::exec_snippet(&proposed_execpolicy_amendment.command)).dim();
            (
                "✔ ".green(),
                vec![
                    actor.subject().into(),
                    "approved".bold(),
                    " chaos to always run commands that start with ".into(),
                    snippet,
                ],
            )
        }
        ApprovedForSession => {
            let snippet = Span::from(render::exec_snippet(&command)).dim();
            (
                "✔ ".green(),
                vec![
                    actor.subject().into(),
                    "approved".bold(),
                    " chaos to run ".into(),
                    snippet,
                    " every time this session".bold(),
                ],
            )
        }
        NetworkPolicyAmendment {
            network_policy_amendment,
        } => match network_policy_amendment.action {
            NetworkPolicyRuleAction::Allow => (
                "✔ ".green(),
                vec![
                    actor.subject().into(),
                    "persisted".bold(),
                    " Chaos network access to ".into(),
                    Span::from(network_policy_amendment.host).dim(),
                ],
            ),
            NetworkPolicyRuleAction::Deny => (
                "✗ ".red(),
                vec![
                    actor.subject().into(),
                    "denied".bold(),
                    " chaos network access to ".into(),
                    Span::from(network_policy_amendment.host).dim(),
                    " and saved that rule".into(),
                ],
            ),
        },
        Denied => {
            let snippet = Span::from(render::exec_snippet(&command)).dim();
            let summary = match actor {
                ApprovalDecisionActor::User => vec![
                    actor.subject().into(),
                    "did not approve".bold(),
                    " chaos to run ".into(),
                    snippet,
                ],
            };
            ("✗ ".red(), summary)
        }
        Abort => {
            let snippet = Span::from(render::exec_snippet(&command)).dim();
            (
                "✗ ".red(),
                vec![
                    actor.subject().into(),
                    "canceled".bold(),
                    " the request to run ".into(),
                    snippet,
                ],
            )
        }
    };

    Box::new(PrefixedWrappedHistoryCell::new(
        Line::from(summary),
        symbol,
        "  ",
    ))
}

/// Cyan history cell line showing the current review status.
pub fn new_review_status_line(message: String) -> PlainHistoryCell {
    PlainHistoryCell {
        lines: vec![Line::from(message.cyan())],
    }
}

// ---------------------------------------------------------------------------
// CompletedMcpToolCallWithImageOutput (private impl detail)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(super) struct CompletedMcpToolCallWithImageOutput {
    pub(super) _image: DynamicImage,
}

impl HistoryCell for CompletedMcpToolCallWithImageOutput {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec!["tool result (image output)".into()]
    }
}

// ---------------------------------------------------------------------------
// SessionInfoCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SessionInfoCell(pub(super) CompositeHistoryCell);

impl HistoryCell for SessionInfoCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.0.display_lines(width)
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.0.desired_height(width)
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.0.transcript_lines(width)
    }
}

pub fn new_session_info(
    _config: &Config,
    requested_model: &str,
    event: SessionConfiguredEvent,
    _is_first_event: bool,
) -> SessionInfoCell {
    let SessionConfiguredEvent { model, .. } = event;
    let mut parts: Vec<Box<dyn HistoryCell>> = Vec::new();

    if requested_model != model {
        let lines = vec![
            "model changed:".magenta().bold().into(),
            format!("requested: {requested_model}").into(),
            format!("used: {model}").into(),
        ];
        parts.push(Box::new(PlainHistoryCell { lines }));
    }

    SessionInfoCell(CompositeHistoryCell { parts })
}

// ---------------------------------------------------------------------------
// CompositeHistoryCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct CompositeHistoryCell {
    pub(super) parts: Vec<Box<dyn HistoryCell>>,
}

impl CompositeHistoryCell {
    pub fn new(parts: Vec<Box<dyn HistoryCell>>) -> Self {
        Self { parts }
    }
}

impl HistoryCell for CompositeHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut first = true;
        for part in &self.parts {
            let mut lines = part.display_lines(width);
            if !lines.is_empty() {
                if !first {
                    out.push(Line::from(""));
                }
                out.append(&mut lines);
                first = false;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// McpToolCallCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct McpToolCallCell {
    call_id: String,
    invocation: McpInvocation,
    start_time: Instant,
    duration: Option<Duration>,
    result: Option<Result<chaos_ipc::mcp::CallToolResult, String>>,
    animations_enabled: bool,
}

impl McpToolCallCell {
    pub fn new(call_id: String, invocation: McpInvocation, animations_enabled: bool) -> Self {
        Self {
            call_id,
            invocation,
            start_time: Instant::now(),
            duration: None,
            result: None,
            animations_enabled,
        }
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn complete(
        &mut self,
        duration: Duration,
        result: Result<chaos_ipc::mcp::CallToolResult, String>,
    ) -> Option<Box<dyn HistoryCell>> {
        let image_cell = render::try_new_completed_mcp_tool_call_with_image_output(&result)
            .map(|cell| Box::new(cell) as Box<dyn HistoryCell>);
        self.duration = Some(duration);
        self.result = Some(result);
        image_cell
    }

    fn success(&self) -> Option<bool> {
        match self.result.as_ref() {
            Some(Ok(result)) => Some(!result.is_error.unwrap_or(false)),
            Some(Err(_)) => Some(false),
            None => None,
        }
    }

    pub fn mark_failed(&mut self) {
        let elapsed = self.start_time.elapsed();
        self.duration = Some(elapsed);
        self.result = Some(Err("interrupted".to_string()));
    }

    fn render_content_block(block: &serde_json::Value, width: usize) -> String {
        let content = match serde_json::from_value::<mcp_guest::ContentBlock>(block.clone()) {
            Ok(content) => content,
            Err(_) => {
                return format_and_truncate_tool_result(
                    &block.to_string(),
                    TOOL_CALL_MAX_LINES,
                    width,
                );
            }
        };

        match content {
            mcp_guest::ContentBlock::Text { text, .. } => {
                format_and_truncate_tool_result(&text, TOOL_CALL_MAX_LINES, width)
            }
            mcp_guest::ContentBlock::Image { .. } => "<image content>".to_string(),
            mcp_guest::ContentBlock::Audio { .. } => "<audio content>".to_string(),
            mcp_guest::ContentBlock::Resource { resource, .. } => {
                let uri = match resource {
                    mcp_guest::ResourceContents::Text(t) => t.uri,
                    mcp_guest::ResourceContents::Blob(b) => b.uri,
                };
                format!("embedded resource: {uri}")
            }
            mcp_guest::ContentBlock::ResourceLink { uri, .. } => format!("link: {uri}"),
        }
    }
}

impl HistoryCell for McpToolCallCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let status = self.success();
        let bullet = match status {
            Some(true) => "•".green().bold(),
            Some(false) => "•".red().bold(),
            None => spinner(Some(self.start_time), self.animations_enabled),
        };
        let header_text = if status.is_some() {
            "Called"
        } else {
            "Calling"
        };

        let invocation_line =
            line_to_static(&render::format_mcp_invocation(self.invocation.clone()));
        let mut compact_spans = vec![bullet.clone(), " ".into(), header_text.bold(), " ".into()];
        let mut compact_header = Line::from(compact_spans.clone());
        let reserved = compact_header.width();

        let inline_invocation =
            invocation_line.width() <= (width as usize).saturating_sub(reserved);

        if inline_invocation {
            compact_header.extend(invocation_line.spans.clone());
            lines.push(compact_header);
        } else {
            compact_spans.pop();
            lines.push(Line::from(compact_spans));

            let opts = RtOptions::new((width as usize).saturating_sub(4))
                .initial_indent("".into())
                .subsequent_indent("    ".into());
            let wrapped = adaptive_wrap_line(&invocation_line, opts);
            let body_lines: Vec<Line<'static>> = wrapped.iter().map(line_to_static).collect();
            lines.extend(prefix_lines(body_lines, "  └ ".dim(), "    ".into()));
        }

        let mut detail_lines: Vec<Line<'static>> = Vec::new();
        let detail_wrap_width = (width as usize).saturating_sub(4).max(1);

        if let Some(result) = &self.result {
            match result {
                Ok(chaos_ipc::mcp::CallToolResult { content, .. }) => {
                    if !content.is_empty() {
                        for block in content {
                            let text = Self::render_content_block(block, detail_wrap_width);
                            for segment in text.split('\n') {
                                let line = Line::from(segment.to_string().dim());
                                let wrapped = adaptive_wrap_line(
                                    &line,
                                    RtOptions::new(detail_wrap_width)
                                        .initial_indent("".into())
                                        .subsequent_indent("    ".into()),
                                );
                                detail_lines.extend(wrapped.iter().map(line_to_static));
                            }
                        }
                    }
                }
                Err(err) => {
                    let err_text = format_and_truncate_tool_result(
                        &format!("Error: {err}"),
                        TOOL_CALL_MAX_LINES,
                        width as usize,
                    );
                    let err_line = Line::from(err_text.dim());
                    let wrapped = adaptive_wrap_line(
                        &err_line,
                        RtOptions::new(detail_wrap_width)
                            .initial_indent("".into())
                            .subsequent_indent("    ".into()),
                    );
                    detail_lines.extend(wrapped.iter().map(line_to_static));
                }
            }
        }

        if !detail_lines.is_empty() {
            let initial_prefix: Span<'static> = if inline_invocation {
                "  └ ".dim()
            } else {
                "    ".into()
            };
            lines.extend(prefix_lines(detail_lines, initial_prefix, "    ".into()));
        }

        lines
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        if !self.animations_enabled || self.result.is_some() {
            return None;
        }
        Some((self.start_time.elapsed().as_millis() / 50) as u64)
    }
}

pub fn new_active_mcp_tool_call(
    call_id: String,
    invocation: McpInvocation,
    animations_enabled: bool,
) -> McpToolCallCell {
    McpToolCallCell::new(call_id, invocation, animations_enabled)
}

// ---------------------------------------------------------------------------
// WebSearchCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct WebSearchCell {
    call_id: String,
    query: String,
    action: Option<WebSearchAction>,
    start_time: Instant,
    completed: bool,
    animations_enabled: bool,
}

impl WebSearchCell {
    pub fn new(
        call_id: String,
        query: String,
        action: Option<WebSearchAction>,
        animations_enabled: bool,
    ) -> Self {
        Self {
            call_id,
            query,
            action,
            start_time: Instant::now(),
            completed: false,
            animations_enabled,
        }
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn update(&mut self, action: WebSearchAction, query: String) {
        self.action = Some(action);
        self.query = query;
    }

    pub fn complete(&mut self) {
        self.completed = true;
    }
}

impl HistoryCell for WebSearchCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let bullet = if self.completed {
            "•".dim()
        } else {
            spinner(Some(self.start_time), self.animations_enabled)
        };
        let header = render::web_search_header(self.completed);
        let detail = web_search_detail(self.action.as_ref(), &self.query);
        let text: Text<'static> = if detail.is_empty() {
            Line::from(vec![header.bold()]).into()
        } else {
            Line::from(vec![header.bold(), " ".into(), detail.into()]).into()
        };
        PrefixedWrappedHistoryCell::new(text, vec![bullet, " ".into()], "  ").display_lines(width)
    }
}

pub fn new_active_web_search_call(
    call_id: String,
    query: String,
    animations_enabled: bool,
) -> WebSearchCell {
    WebSearchCell::new(call_id, query, None, animations_enabled)
}

pub fn new_web_search_call(
    call_id: String,
    query: String,
    action: WebSearchAction,
) -> WebSearchCell {
    let mut cell = WebSearchCell::new(call_id, query, Some(action), false);
    cell.complete();
    cell
}

// ---------------------------------------------------------------------------
// MCP tools output cells
// ---------------------------------------------------------------------------

#[allow(clippy::disallowed_methods)]
pub fn new_warning_event(message: String) -> PrefixedWrappedHistoryCell {
    PrefixedWrappedHistoryCell::new(message.yellow(), "⚠ ".yellow(), "  ")
}

#[derive(Debug)]
pub struct DeprecationNoticeCell {
    summary: String,
    details: Option<String>,
}

pub fn new_deprecation_notice(summary: String, details: Option<String>) -> DeprecationNoticeCell {
    DeprecationNoticeCell { summary, details }
}

impl HistoryCell for DeprecationNoticeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(vec!["⚠ ".red().bold(), self.summary.clone().red()].into());

        let wrap_width = width.saturating_sub(4).max(1) as usize;

        if let Some(details) = &self.details {
            let detail_line = Line::from(details.clone().dim());
            let wrapped = adaptive_wrap_line(&detail_line, RtOptions::new(wrap_width));
            push_owned_lines(&wrapped, &mut lines);
        }

        lines
    }
}

/// Render a summary of configured MCP servers from the current `Config`.
pub fn empty_mcp_output() -> PlainHistoryCell {
    let lines: Vec<Line<'static>> = vec![
        "/mcp".magenta().into(),
        "".into(),
        vec!["🔌  ".into(), "MCP Tools".bold()].into(),
        "".into(),
        "  • No MCP servers configured.".italic().into(),
        Line::from(vec![
            "    See the ".into(),
            format!(
                "\u{1b}]8;;{}\u{7}MCP docs\u{1b}]8;;\u{7}",
                chaos_services::openai::DEVELOPERS_MCP_DOCS,
            )
            .underlined(),
            " to configure them.".into(),
        ])
        .style(Style::default().add_modifier(Modifier::DIM)),
    ];

    PlainHistoryCell { lines }
}

/// Render MCP tools grouped by connection using the fully-qualified tool names.
pub fn new_mcp_tools_output(
    config: &Config,
    tools: HashMap<String, chaos_ipc::mcp::Tool>,
    resources: HashMap<String, Vec<Resource>>,
    resource_templates: HashMap<String, Vec<ResourceTemplate>>,
    auth_statuses: &HashMap<String, McpAuthStatus>,
) -> PlainHistoryCell {
    use crate::tool_badges::tool_name_style;
    use crate::tool_badges::tool_name_style_from_annotations;

    let mut lines: Vec<Line<'static>> = vec![
        "/mcp".magenta().into(),
        "".into(),
        vec!["🔌  ".into(), "MCP Tools".bold()].into(),
        "".into(),
    ];

    if tools.is_empty() {
        lines.push("  • No MCP tools available.".italic().into());
        lines.push("".into());
    }

    let mcp_manager = McpManager::new();
    let effective_servers = mcp_manager.effective_servers(config);
    let mut servers: Vec<_> = effective_servers.iter().collect();
    servers.sort_by(|(a, _), (b, _)| a.cmp(b));

    for (server, cfg) in servers {
        let prefix = format!("mcp__{server}__");
        let mut tool_entries: Vec<(String, chaos_ipc::mcp::Tool)> = tools
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .filter_map(|k| {
                tools
                    .get(k)
                    .cloned()
                    .map(|tool| (k[prefix.len()..].to_string(), tool))
            })
            .collect();
        tool_entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let auth_status = auth_statuses
            .get(server.as_str())
            .copied()
            .unwrap_or(McpAuthStatus::Unsupported);
        let mut header: Vec<Span<'static>> = vec!["  • ".into(), server.clone().into()];
        if !cfg.enabled {
            header.push(" ".into());
            header.push("(disabled)".red());
            lines.push(header.into());
            if let Some(reason) = cfg.disabled_reason.as_ref().map(ToString::to_string) {
                lines.push(vec!["    • Reason: ".into(), reason.dim()].into());
            }
            lines.push(Line::from(""));
            continue;
        }
        lines.push(header.into());
        lines.push(vec!["    • Status: ".into(), "enabled".green()].into());
        if matches!(
            cfg.transport,
            McpServerTransportConfig::StreamableHttp { .. }
        ) && auth_status != McpAuthStatus::Unsupported
        {
            lines.push(vec!["    • Auth: ".into(), auth_status.to_string().into()].into());
        }

        match &cfg.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                env,
                env_vars,
                cwd,
            } => {
                let args_suffix = if args.is_empty() {
                    String::new()
                } else {
                    format!(" {}", args.join(" "))
                };
                let cmd_display = format!("{command}{args_suffix}");
                lines.push(vec!["    • Command: ".into(), cmd_display.into()].into());

                if let Some(cwd) = cwd.as_ref() {
                    lines.push(vec!["    • Cwd: ".into(), cwd.display().to_string().into()].into());
                }

                let env_display = format_env_display(env.as_ref(), env_vars);
                if env_display != "-" {
                    lines.push(vec!["    • Env: ".into(), env_display.into()].into());
                }
            }
            McpServerTransportConfig::StreamableHttp {
                url,
                http_headers,
                env_http_headers,
                ..
            } => {
                lines.push(vec!["    • URL: ".into(), url.clone().into()].into());
                if let Some(headers) = http_headers.as_ref()
                    && !headers.is_empty()
                {
                    let mut pairs: Vec<_> = headers.iter().collect();
                    pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
                    let display = pairs
                        .into_iter()
                        .map(|(name, _)| format!("{name}=*****"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push(vec!["    • HTTP headers: ".into(), display.into()].into());
                }
                if let Some(headers) = env_http_headers.as_ref()
                    && !headers.is_empty()
                {
                    let mut pairs: Vec<_> = headers.iter().collect();
                    pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
                    let display = pairs
                        .into_iter()
                        .map(|(name, var)| format!("{name}={var}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push(vec!["    • Env HTTP headers: ".into(), display.into()].into());
                }
            }
        }

        if tool_entries.is_empty() {
            lines.push("    • Tools: (none)".into());
        } else {
            lines.push("    • Tools:".into());
            for (tool_name, tool) in tool_entries {
                let name_style = if tool.annotations.is_some() {
                    tool_name_style_from_annotations(tool.annotations.as_ref())
                } else {
                    tool_name_style()
                };
                let mut spans: Vec<Span<'static>> =
                    vec!["      - ".into(), Span::styled(tool_name, name_style)];
                if let Some(description) = tool.description.filter(|d| !d.is_empty()) {
                    spans.push(" ".into());
                    spans.push(format!("— {description}").dim());
                }
                lines.push(spans.into());
            }
        }

        let server_resources: Vec<Resource> =
            resources.get(server.as_str()).cloned().unwrap_or_default();
        if server_resources.is_empty() {
            lines.push("    • Resources: (none)".into());
        } else {
            let mut spans: Vec<Span<'static>> = vec!["    • Resources: ".into()];

            for (idx, resource) in server_resources.iter().enumerate() {
                if idx > 0 {
                    spans.push(", ".into());
                }

                let label = resource.title.as_ref().unwrap_or(&resource.name);
                spans.push(label.clone().into());
                spans.push(" ".into());
                spans.push(format!("({})", resource.uri).dim());
            }

            lines.push(spans.into());
        }

        let server_templates: Vec<ResourceTemplate> = resource_templates
            .get(server.as_str())
            .cloned()
            .unwrap_or_default();
        if server_templates.is_empty() {
            lines.push("    • Resource templates: (none)".into());
        } else {
            let mut spans: Vec<Span<'static>> = vec!["    • Resource templates: ".into()];

            for (idx, template) in server_templates.iter().enumerate() {
                if idx > 0 {
                    spans.push(", ".into());
                }

                let label = template.title.as_ref().unwrap_or(&template.name);
                spans.push(label.clone().into());
                spans.push(" ".into());
                spans.push(format!("({})", template.uri_template).dim());
            }

            lines.push(spans.into());
        }

        lines.push(Line::from(""));
    }

    PlainHistoryCell { lines }
}

pub fn new_info_event(message: String, hint: Option<String>) -> PlainHistoryCell {
    let mut line = vec!["• ".dim(), message.into()];
    if let Some(hint) = hint {
        line.push(" ".into());
        line.push(hint.dark_gray());
    }
    let lines: Vec<Line<'static>> = vec![line.into()];
    PlainHistoryCell { lines }
}

pub fn new_error_event(message: String) -> PlainHistoryCell {
    let lines: Vec<Line<'static>> = vec![vec![format!("■ {message}").red()].into()];
    PlainHistoryCell { lines }
}

// ---------------------------------------------------------------------------
// RequestUserInputResultCell
// ---------------------------------------------------------------------------

/// Renders a completed (or interrupted) request_user_input exchange in history.
#[derive(Debug)]
pub struct RequestUserInputResultCell {
    pub questions: Vec<RequestUserInputQuestion>,
    pub answers: HashMap<String, RequestUserInputAnswer>,
    pub interrupted: bool,
}

impl HistoryCell for RequestUserInputResultCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let width = width.max(1) as usize;
        let total = self.questions.len();
        let answered = self
            .questions
            .iter()
            .filter(|question| {
                self.answers
                    .get(&question.id)
                    .is_some_and(|answer| !answer.answers.is_empty())
            })
            .count();
        let unanswered = total.saturating_sub(answered);

        let mut header = vec!["•".dim(), " ".into(), "Questions".bold()];
        header.push(format!(" {answered}/{total} answered").dim());
        if self.interrupted {
            header.push(" (interrupted)".cyan());
        }

        let mut lines: Vec<Line<'static>> = vec![header.into()];

        for question in &self.questions {
            let answer = self.answers.get(&question.id);
            let answer_missing = match answer {
                Some(answer) => answer.answers.is_empty(),
                None => true,
            };
            let mut question_lines = render::wrap_with_prefix(
                &question.question,
                width,
                "  • ".into(),
                "    ".into(),
                Style::default(),
            );
            if answer_missing && let Some(last) = question_lines.last_mut() {
                last.spans.push(" (unanswered)".dim());
            }
            lines.extend(question_lines);

            let Some(answer) = answer.filter(|answer| !answer.answers.is_empty()) else {
                continue;
            };
            if question.is_secret {
                lines.extend(render::wrap_with_prefix(
                    "••••••",
                    width,
                    "    answer: ".dim(),
                    "            ".dim(),
                    Style::default().fg(crate::theme::cyan()),
                ));
                continue;
            }

            let (options, note) = render::split_request_user_input_answer(answer);

            for option in options {
                lines.extend(render::wrap_with_prefix(
                    &option,
                    width,
                    "    answer: ".dim(),
                    "            ".dim(),
                    Style::default().fg(crate::theme::cyan()),
                ));
            }
            if let Some(note) = note {
                let (label, continuation, style) = if question.options.is_some() {
                    (
                        "    note: ".dim(),
                        "          ".dim(),
                        Style::default().fg(crate::theme::cyan()),
                    )
                } else {
                    (
                        "    answer: ".dim(),
                        "            ".dim(),
                        Style::default().fg(crate::theme::cyan()),
                    )
                };
                lines.extend(render::wrap_with_prefix(
                    &note,
                    width,
                    label,
                    continuation,
                    style,
                ));
            }
        }

        if self.interrupted && unanswered > 0 {
            let summary = format!("interrupted with {unanswered} unanswered");
            lines.extend(render::wrap_with_prefix(
                &summary,
                width,
                "  ↳ ".cyan().dim(),
                "    ".dim(),
                Style::default()
                    .fg(crate::theme::cyan())
                    .add_modifier(Modifier::DIM),
            ));
        }

        lines
    }
}

// ---------------------------------------------------------------------------
// Plan cells
// ---------------------------------------------------------------------------

/// Render a user-friendly plan update styled like a checkbox todo list.
pub fn new_plan_update(update: UpdatePlanArgs) -> PlanUpdateCell {
    let UpdatePlanArgs { explanation, plan } = update;
    PlanUpdateCell { explanation, plan }
}

/// Create a proposed-plan cell that snapshots the session cwd for later markdown rendering.
pub fn new_proposed_plan(plan_markdown: String, cwd: &Path) -> ProposedPlanCell {
    ProposedPlanCell {
        plan_markdown,
        cwd: cwd.to_path_buf(),
    }
}

pub fn new_proposed_plan_stream(
    lines: Vec<Line<'static>>,
    is_stream_continuation: bool,
) -> ProposedPlanStreamCell {
    ProposedPlanStreamCell {
        lines,
        is_stream_continuation,
    }
}

#[derive(Debug)]
pub struct ProposedPlanCell {
    plan_markdown: String,
    /// Session cwd used to keep local file-link display aligned with live streamed plan rendering.
    cwd: PathBuf,
}

#[derive(Debug)]
pub struct ProposedPlanStreamCell {
    lines: Vec<Line<'static>>,
    is_stream_continuation: bool,
}

impl HistoryCell for ProposedPlanCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(vec!["• ".dim(), "Proposed Plan".bold()].into());
        lines.push(Line::from(" "));

        let mut plan_lines: Vec<Line<'static>> = vec![Line::from(" ")];
        let plan_style = proposed_plan_style();
        let wrap_width = width.saturating_sub(4).max(1) as usize;
        let mut body: Vec<Line<'static>> = Vec::new();
        append_markdown(
            &self.plan_markdown,
            Some(wrap_width),
            Some(self.cwd.as_path()),
            &mut body,
        );
        if body.is_empty() {
            body.push(Line::from("(empty)".dim().italic()));
        }
        plan_lines.extend(prefix_lines(body, "  ".into(), "  ".into()));
        plan_lines.push(Line::from(" "));

        lines.extend(plan_lines.into_iter().map(|line| line.style(plan_style)));
        lines
    }
}

impl HistoryCell for ProposedPlanStreamCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }

    fn is_stream_continuation(&self) -> bool {
        self.is_stream_continuation
    }
}

#[derive(Debug)]
pub struct PlanUpdateCell {
    explanation: Option<String>,
    plan: Vec<PlanItemArg>,
}

impl HistoryCell for PlanUpdateCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let render_note = |text: &str| -> Vec<Line<'static>> {
            let wrap_width = width.saturating_sub(4).max(1) as usize;
            let note = Line::from(text.to_string().dim().italic());
            let wrapped = adaptive_wrap_line(&note, RtOptions::new(wrap_width));
            let mut out = Vec::new();
            push_owned_lines(&wrapped, &mut out);
            out
        };

        let render_step = |status: &StepStatus, text: &str| -> Vec<Line<'static>> {
            let (box_str, step_style) = match status {
                StepStatus::Completed => ("✔ ", Style::default().crossed_out().dim()),
                StepStatus::InProgress => ("□ ", Style::default().cyan().bold()),
                StepStatus::Pending => ("□ ", Style::default().dim()),
            };

            let opts = RtOptions::new(width.saturating_sub(4).max(1) as usize)
                .initial_indent(box_str.into())
                .subsequent_indent("  ".into());
            let step = Line::from(text.to_string().set_style(step_style));
            let wrapped = adaptive_wrap_line(&step, opts);
            let mut out = Vec::new();
            push_owned_lines(&wrapped, &mut out);
            out
        };

        let mut lines: Vec<Line<'static>> = vec![];
        lines.push(vec!["• ".dim(), "Updated Plan".bold()].into());

        let mut indented_lines = vec![];
        let note = self
            .explanation
            .as_ref()
            .map(|s| s.trim())
            .filter(|t| !t.is_empty());
        if let Some(expl) = note {
            indented_lines.extend(render_note(expl));
        };

        if self.plan.is_empty() {
            indented_lines.push(Line::from("(no steps provided)".dim().italic()));
        } else {
            for PlanItemArg { step, status } in self.plan.iter() {
                indented_lines.extend(render_step(status, step));
            }
        }
        lines.extend(prefix_lines(indented_lines, "  └ ".dim(), "    ".into()));

        lines
    }
}

// ---------------------------------------------------------------------------
// Image generation cell
// ---------------------------------------------------------------------------

pub fn new_image_generation_call(
    call_id: String,
    revised_prompt: Option<String>,
    saved_to: Option<String>,
) -> PlainHistoryCell {
    let detail = revised_prompt.unwrap_or_else(|| call_id.clone());

    let mut lines: Vec<Line<'static>> = vec![
        vec!["• ".dim(), "Generated Image:".bold()].into(),
        vec!["  └ ".dim(), detail.dim()].into(),
    ];
    if let Some(saved_to) = saved_to {
        lines.push(vec!["  └ ".dim(), format!("Saved to: {saved_to}").dim()].into());
    }

    PlainHistoryCell { lines }
}

// ---------------------------------------------------------------------------
// FinalMessageSeparator
// ---------------------------------------------------------------------------

/// A visual divider between turns, optionally showing how long the assistant worked.
#[derive(Debug)]
pub struct FinalMessageSeparator {
    elapsed_seconds: Option<u64>,
    runtime_metrics: Option<RuntimeMetricsSummary>,
}

impl FinalMessageSeparator {
    pub fn new(
        elapsed_seconds: Option<u64>,
        runtime_metrics: Option<RuntimeMetricsSummary>,
    ) -> Self {
        Self {
            elapsed_seconds,
            runtime_metrics,
        }
    }
}

impl HistoryCell for FinalMessageSeparator {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut label_parts = Vec::new();
        if let Some(elapsed_seconds) = self
            .elapsed_seconds
            .filter(|seconds| *seconds > 60)
            .map(super::super::status_indicator_widget::fmt_elapsed_compact)
        {
            label_parts.push(format!("Worked for {elapsed_seconds}"));
        }
        if let Some(metrics_label) = self.runtime_metrics.and_then(render::runtime_metrics_label) {
            label_parts.push(metrics_label);
        }

        if label_parts.is_empty() {
            return vec![Line::from_iter(["─".repeat(width as usize).dim()])];
        }

        let label = format!("─ {} ─", label_parts.join(" • "));
        let (label, _suffix, label_width) = take_prefix_by_width(&label, width as usize);
        vec![
            Line::from_iter([
                label,
                "─".repeat((width as usize).saturating_sub(label_width)),
            ])
            .dim(),
        ]
    }
}
