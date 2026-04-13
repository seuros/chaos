use pulldown_cmark::{BlockQuoteKind, CowStr, Event};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use std::path::{Path, PathBuf};

use crate::render::line_utils::line_to_static;
use crate::wrapping::{RtOptions, adaptive_wrap_line};

use super::line_utils::is_local_path_like_link;
use super::styles::MarkdownStyles;

#[derive(Clone, Debug)]
pub(super) struct IndentContext {
    pub(super) prefix: Vec<Span<'static>>,
    pub(super) marker: Option<Vec<Span<'static>>>,
    pub(super) is_list: bool,
}

impl IndentContext {
    pub(super) fn new(
        prefix: Vec<Span<'static>>,
        marker: Option<Vec<Span<'static>>>,
        is_list: bool,
    ) -> Self {
        Self {
            prefix,
            marker,
            is_list,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct LinkState {
    pub(super) destination: String,
    pub(super) show_destination: bool,
    /// Pre-rendered display text for local file links.
    pub(super) local_target_display: Option<String>,
}

pub(super) fn should_render_link_destination(dest_url: &str) -> bool {
    !is_local_path_like_link(dest_url)
}

/// Rewrite LaTeX math source into Unicode glyphs via the `unicodeit` port.
pub(super) fn rewrite_math(source: &str) -> CowStr<'static> {
    let rewritten = unicodeit::replace(source);
    CowStr::Boxed(rewritten.into_boxed_str())
}

/// Glyph, label, and style for a GFM alert kind.
pub(super) fn alert_header_style(kind: BlockQuoteKind) -> (&'static str, &'static str, Style) {
    let p = crate::theme::palette();
    match kind {
        BlockQuoteKind::Note => ("ⓘ", "NOTE", Style::new().fg(p.accent).bold()),
        BlockQuoteKind::Tip => ("★", "TIP", Style::new().fg(p.success).bold()),
        BlockQuoteKind::Important => ("‼", "IMPORTANT", Style::new().fg(p.accent).bold()),
        BlockQuoteKind::Warning => ("⚠", "WARNING", Style::new().fg(p.warning).bold()),
        BlockQuoteKind::Caution => ("⛔", "CAUTION", Style::new().fg(p.error).bold()),
    }
}

pub(super) struct Writer<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    pub(super) iter: I,
    pub(super) text: Text<'static>,
    pub(super) styles: MarkdownStyles,
    pub(super) inline_styles: Vec<Style>,
    pub(super) indent_stack: Vec<IndentContext>,
    pub(super) list_indices: Vec<Option<u64>>,
    pub(super) link: Option<LinkState>,
    /// OSC 8 sentinel stamped onto spans while a `Tag::Link` is open.
    pub(super) active_link_sentinel: Option<Color>,
    pub(super) needs_newline: bool,
    pub(super) pending_marker_line: bool,
    pub(super) in_paragraph: bool,
    pub(super) in_code_block: bool,
    pub(super) code_block_lang: Option<String>,
    pub(super) code_block_buffer: String,
    pub(super) wrap_width: Option<usize>,
    pub(super) cwd: Option<PathBuf>,
    pub(super) line_ends_with_local_link_target: bool,
    pub(super) pending_local_link_soft_break: bool,
    pub(super) current_line_content: Option<Line<'static>>,
    pub(super) current_initial_indent: Vec<Span<'static>>,
    pub(super) current_subsequent_indent: Vec<Span<'static>>,
    pub(super) current_line_style: Style,
    pub(super) current_line_in_code_block: bool,
}

impl<'a, I> Writer<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    pub(super) fn new(iter: I, wrap_width: Option<usize>, cwd: Option<&Path>) -> Self {
        Self {
            iter,
            text: Text::default(),
            styles: MarkdownStyles::default(),
            inline_styles: Vec::new(),
            indent_stack: Vec::new(),
            list_indices: Vec::new(),
            link: None,
            active_link_sentinel: None,
            needs_newline: false,
            pending_marker_line: false,
            in_paragraph: false,
            in_code_block: false,
            code_block_lang: None,
            code_block_buffer: String::new(),
            wrap_width,
            cwd: cwd.map(Path::to_path_buf),
            line_ends_with_local_link_target: false,
            pending_local_link_soft_break: false,
            current_line_content: None,
            current_initial_indent: Vec::new(),
            current_subsequent_indent: Vec::new(),
            current_line_style: Style::default(),
            current_line_in_code_block: false,
        }
    }

    pub(super) fn run(&mut self) {
        while let Some(ev) = self.iter.next() {
            self.handle_event(ev);
        }
        self.flush_current_line();
    }

    pub(super) fn handle_event(&mut self, event: Event<'a>) {
        self.prepare_for_event(&event);
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.text(text),
            Event::Code(code) => self.code(code),
            Event::InlineMath(math) => {
                let math = rewrite_math(&math);
                self.code_lines(&math);
            }
            Event::DisplayMath(math) => {
                let math = rewrite_math(&math);
                self.code_lines(&math);
                self.needs_newline = true;
            }
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => {
                self.flush_current_line();
                if !self.text.lines.is_empty() {
                    self.push_blank_line();
                }
                self.push_line(Line::from("———"));
                self.needs_newline = true;
            }
            Event::Html(html) => self.html(html, /*inline*/ false),
            Event::InlineHtml(html) => self.html(html, /*inline*/ true),
            Event::FootnoteReference(_) => {}
            Event::TaskListMarker(checked) => self.task_list_marker(checked),
        }
    }

    pub(super) fn prepare_for_event(&mut self, event: &Event<'a>) {
        if !self.pending_local_link_soft_break {
            return;
        }

        if matches!(event, Event::Text(text) if text.trim_start().starts_with(':')) {
            self.pending_local_link_soft_break = false;
            return;
        }

        self.pending_local_link_soft_break = false;
        self.push_line(Line::default());
    }

    pub(super) fn flush_current_line(&mut self) {
        if let Some(line) = self.current_line_content.take() {
            let style = self.current_line_style;
            // NB we don't wrap code in code blocks, in order to preserve whitespace for copy/paste.
            if !self.current_line_in_code_block
                && let Some(width) = self.wrap_width
            {
                let opts = RtOptions::new(width)
                    .initial_indent(self.current_initial_indent.clone().into())
                    .subsequent_indent(self.current_subsequent_indent.clone().into());
                for wrapped in adaptive_wrap_line(&line, opts) {
                    let owned = line_to_static(&wrapped).style(style);
                    self.text.lines.push(owned);
                }
            } else {
                let mut spans = self.current_initial_indent.clone();
                let mut line = line;
                spans.append(&mut line.spans);
                self.text.lines.push(Line::from_iter(spans).style(style));
            }
            self.current_initial_indent.clear();
            self.current_subsequent_indent.clear();
            self.current_line_in_code_block = false;
            self.line_ends_with_local_link_target = false;
        }
    }

    pub(super) fn push_line(&mut self, line: Line<'static>) {
        self.flush_current_line();
        let blockquote_active = self
            .indent_stack
            .iter()
            .any(|ctx| ctx.prefix.iter().any(|s| s.content.contains('>')));
        let style = if blockquote_active {
            self.styles.blockquote
        } else {
            line.style
        };
        let was_pending = self.pending_marker_line;

        self.current_initial_indent = self.prefix_spans(was_pending);
        self.current_subsequent_indent = self.prefix_spans(/*pending_marker_line*/ false);
        self.current_line_style = style;
        self.current_line_content = Some(line);
        self.current_line_in_code_block = self.in_code_block;
        self.line_ends_with_local_link_target = false;

        self.pending_marker_line = false;
    }

    pub(super) fn push_span(&mut self, mut span: Span<'static>) {
        if let Some(sentinel) = self.active_link_sentinel
            && span.style.underline_color.is_none()
        {
            span.style.underline_color = Some(sentinel);
        }
        if let Some(line) = self.current_line_content.as_mut() {
            line.push_span(span);
        } else {
            self.push_line(Line::from(vec![span]));
        }
    }

    pub(super) fn push_blank_line(&mut self) {
        self.flush_current_line();
        if self.indent_stack.iter().all(|ctx| ctx.is_list) {
            self.text.lines.push(Line::default());
        } else {
            self.push_line(Line::default());
            self.flush_current_line();
        }
    }

    pub(super) fn prefix_spans(&self, pending_marker_line: bool) -> Vec<Span<'static>> {
        let mut prefix: Vec<Span<'static>> = Vec::new();
        let last_marker_index = if pending_marker_line {
            self.indent_stack
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, ctx)| if ctx.marker.is_some() { Some(i) } else { None })
        } else {
            None
        };
        let last_list_index = self.indent_stack.iter().rposition(|ctx| ctx.is_list);

        for (i, ctx) in self.indent_stack.iter().enumerate() {
            if pending_marker_line {
                if Some(i) == last_marker_index
                    && let Some(marker) = &ctx.marker
                {
                    prefix.extend(marker.iter().cloned());
                    continue;
                }
                if ctx.is_list && last_marker_index.is_some_and(|idx| idx > i) {
                    continue;
                }
            } else if ctx.is_list && Some(i) != last_list_index {
                continue;
            }
            prefix.extend(ctx.prefix.iter().cloned());
        }

        prefix
    }
}
