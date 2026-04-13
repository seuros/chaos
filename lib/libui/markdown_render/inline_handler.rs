use pulldown_cmark::{CowStr, Event, LinkType};
use ratatui::text::{Line, Span};

use crate::osc8;

use super::line_utils::{
    file_url_for_local_link, is_local_path_like_link, render_local_link_target,
};
use super::writer::{LinkState, Writer, should_render_link_destination};

impl<'a, I> Writer<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    /// Inject a `[x]` / `[ ]` checkbox into the current list item's marker.
    ///
    /// `Event::TaskListMarker` is emitted by pulldown-cmark after `Tag::Item`
    /// and before the item's text content. At this point the item's
    /// `IndentContext` is already on the top of `indent_stack` carrying its
    /// bullet marker (e.g. `- `). We append the checkbox glyph to that marker
    /// so the initial line renders as `- [ ] task text` in one prefix pass,
    /// and pad the continuation prefix by the same display width so wrapped
    /// lines align under the text, not under the checkbox.
    pub(super) fn task_list_marker(&mut self, checked: bool) {
        let glyph = if checked { "[x] " } else { "[ ] " };
        let style = if checked {
            self.styles.task_checked
        } else {
            self.styles.task_unchecked
        };
        let marker_span = Span::styled(glyph.to_string(), style);
        let continuation_span = Span::from(" ".repeat(glyph.len()));
        if let Some(ctx) = self.indent_stack.last_mut() {
            match &mut ctx.marker {
                Some(marker) => marker.push(marker_span),
                None => ctx.marker = Some(vec![marker_span]),
            }
            ctx.prefix.push(continuation_span);
        }

        if self.pending_marker_line {
            self.push_line(Line::default());
        } else if self
            .current_line_content
            .as_ref()
            .is_some_and(|line| line.spans.is_empty())
        {
            self.current_initial_indent = self.prefix_spans(/*pending_marker_line*/ true);
            self.current_subsequent_indent = self.prefix_spans(/*pending_marker_line*/ false);
        }
    }

    pub(super) fn text(&mut self, text: CowStr<'a>) {
        if self.suppressing_local_link_label() {
            return;
        }
        self.line_ends_with_local_link_target = false;
        if self.pending_marker_line {
            self.push_line(Line::default());
        }
        self.pending_marker_line = false;

        // When inside a fenced code block with a known language, accumulate
        // text into the buffer for batch highlighting in end_codeblock().
        // Append verbatim — pulldown-cmark text events already contain the
        // original line breaks, so inserting separators would double them.
        if self.in_code_block && self.code_block_lang.is_some() {
            self.code_block_buffer.push_str(&text);
            return;
        }

        if self.in_code_block && !self.needs_newline {
            let has_content = self
                .current_line_content
                .as_ref()
                .map(|line| !line.spans.is_empty())
                .unwrap_or_else(|| {
                    self.text
                        .lines
                        .last()
                        .map(|line| !line.spans.is_empty())
                        .unwrap_or(false)
                });
            if has_content {
                self.push_line(Line::default());
            }
        }
        for (i, line) in text.lines().enumerate() {
            if self.needs_newline {
                self.push_line(Line::default());
                self.needs_newline = false;
            }
            if i > 0 {
                self.push_line(Line::default());
            }
            let content = line.to_string();
            let span = Span::styled(
                content,
                self.inline_styles.last().copied().unwrap_or_default(),
            );
            self.push_span(span);
        }
        self.needs_newline = false;
    }

    pub(super) fn code(&mut self, code: CowStr<'a>) {
        if self.suppressing_local_link_label() {
            return;
        }
        self.line_ends_with_local_link_target = false;
        if self.pending_marker_line {
            self.push_line(Line::default());
            self.pending_marker_line = false;
        }
        let span = Span::from(code.into_string()).style(self.styles.code);
        self.push_span(span);
    }

    pub(super) fn code_lines(&mut self, code: &str) {
        if self.suppressing_local_link_label() {
            return;
        }
        self.line_ends_with_local_link_target = false;
        if self.pending_marker_line {
            self.push_line(Line::default());
            self.pending_marker_line = false;
        }
        for (i, line) in code.lines().enumerate() {
            if self.needs_newline {
                self.push_line(Line::default());
                self.needs_newline = false;
            }
            if i > 0 {
                self.push_line(Line::default());
            }
            let span = Span::from(line.to_string()).style(self.styles.code);
            self.push_span(span);
        }
    }

    pub(super) fn html(&mut self, html: CowStr<'a>, inline: bool) {
        if self.suppressing_local_link_label() {
            return;
        }
        self.line_ends_with_local_link_target = false;
        self.pending_marker_line = false;
        for (i, line) in html.lines().enumerate() {
            if self.needs_newline {
                self.push_line(Line::default());
                self.needs_newline = false;
            }
            if i > 0 {
                self.push_line(Line::default());
            }
            let style = self.inline_styles.last().copied().unwrap_or_default();
            self.push_span(Span::styled(line.to_string(), style));
        }
        self.needs_newline = !inline;
    }

    pub(super) fn hard_break(&mut self) {
        if self.suppressing_local_link_label() {
            return;
        }
        self.line_ends_with_local_link_target = false;
        self.push_line(Line::default());
    }

    pub(super) fn soft_break(&mut self) {
        if self.suppressing_local_link_label() {
            return;
        }
        if self.line_ends_with_local_link_target {
            self.pending_local_link_soft_break = true;
            self.line_ends_with_local_link_target = false;
            return;
        }
        self.line_ends_with_local_link_target = false;
        self.push_line(Line::default());
    }

    pub(super) fn push_inline_style(&mut self, style: ratatui::style::Style) {
        let current = self.inline_styles.last().copied().unwrap_or_default();
        let merged = current.patch(style);
        self.inline_styles.push(merged);
    }

    pub(super) fn pop_inline_style(&mut self) {
        self.inline_styles.pop();
    }

    pub(super) fn push_link(&mut self, link_type: LinkType, dest_url: String) {
        let show_destination = should_render_link_destination(&dest_url)
            && !matches!(link_type, LinkType::Autolink | LinkType::Email);
        let local_target_display = if is_local_path_like_link(&dest_url) {
            render_local_link_target(&dest_url, self.cwd.as_deref())
        } else {
            None
        };
        let osc8_url = if is_local_path_like_link(&dest_url) {
            file_url_for_local_link(&dest_url, self.cwd.as_deref())
        } else {
            Some(dest_url.clone())
        };
        self.active_link_sentinel = osc8_url.as_deref().map(osc8::register);
        self.link = Some(LinkState {
            show_destination,
            local_target_display,
            destination: dest_url,
        });
    }

    pub(super) fn pop_link(&mut self) {
        if let Some(link) = self.link.take() {
            if link.show_destination {
                self.push_span(" (".into());
                self.push_span(Span::styled(link.destination, self.styles.link));
                self.push_span(")".into());
            } else if let Some(local_target_display) = link.local_target_display {
                if self.pending_marker_line {
                    self.push_line(Line::default());
                }
                let style = self
                    .inline_styles
                    .last()
                    .copied()
                    .unwrap_or_default()
                    .patch(self.styles.code);
                self.push_span(Span::styled(local_target_display, style));
                self.line_ends_with_local_link_target = true;
            }
        }
        self.active_link_sentinel = None;
    }

    pub(super) fn suppressing_local_link_label(&self) -> bool {
        self.link
            .as_ref()
            .and_then(|link| link.local_target_display.as_ref())
            .is_some()
    }
}
