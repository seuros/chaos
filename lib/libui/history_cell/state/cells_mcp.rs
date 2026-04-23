use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use crate::exec_cell::TOOL_CALL_MAX_LINES;
use crate::exec_cell::spinner;
use crate::render::line_utils::line_to_static;
use crate::render::line_utils::prefix_lines;
use crate::text_formatting::format_and_truncate_tool_result;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use chaos_getopt::format_env_display::format_env_display;
use chaos_ipc::mcp::Resource;
use chaos_ipc::mcp::ResourceTemplate;
use chaos_ipc::models::WebSearchAction;
use chaos_ipc::protocol::McpAuthStatus;
use chaos_ipc::protocol::McpInvocation;
use chaos_ipc::request_user_input::RequestUserInputAnswer;
use chaos_ipc::request_user_input::RequestUserInputQuestion;
use chaos_kern::config::Config;
use chaos_kern::config::types::McpServerTransportConfig;
use chaos_kern::mcp::McpManager;
use chaos_kern::web_search::web_search_detail;
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;

use super::super::render;
use super::cells_basic::PlainHistoryCell;
use super::cells_basic::PrefixedWrappedHistoryCell;
use super::trait_def::HistoryCell;

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
// DeprecationNoticeCell
// ---------------------------------------------------------------------------

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
            crate::render::line_utils::push_owned_lines(&wrapped, &mut lines);
        }

        lines
    }
}

// ---------------------------------------------------------------------------
// MCP tools output free fns
// ---------------------------------------------------------------------------

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

    PlainHistoryCell::new(lines)
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
    servers.sort_by_key(|(a, _)| *a);

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
        tool_entries.sort_by_key(|(a, _)| a.clone());

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
                    pairs.sort_by_key(|(a, _)| *a);
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
                    pairs.sort_by_key(|(a, _)| *a);
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

    PlainHistoryCell::new(lines)
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
