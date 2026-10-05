use pulldown_cmark::{BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};
use ratatui::text::{Line, Span};

use crate::render::highlight::highlight_code_to_lines;

use super::writer::{IndentContext, Writer, alert_header_style};

impl<'a, I> Writer<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    pub(super) fn start_tag(&mut self, tag: Tag<'a>) {
        match tag {
            Tag::Paragraph => self.start_paragraph(),
            Tag::Heading {
                level,
                id,
                classes,
                attrs,
            } => {
                self.start_heading(level);
                let mut parts = Vec::new();
                if let Some(id) = id {
                    parts.push(format!("#{id}"));
                }
                parts.extend(classes.iter().map(|class| format!(".{class}")));
                parts.extend(attrs.iter().map(|(key, value)| match value {
                    Some(value) => format!("{key}={value}"),
                    None => key.to_string(),
                }));
                self.heading_suffix =
                    (!parts.is_empty()).then(|| format!(" {{{}}}", parts.join(" ")));
            }
            Tag::BlockQuote(kind) => self.start_blockquote(kind),
            Tag::CodeBlock(kind) => {
                let indent = match kind {
                    CodeBlockKind::Fenced(_) => None,
                    CodeBlockKind::Indented => Some(Span::from(" ".repeat(4))),
                };
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => Some(lang.to_string()),
                    CodeBlockKind::Indented => None,
                };
                self.start_codeblock(lang, indent)
            }
            Tag::List(start) => self.start_list(start),
            Tag::Item => self.start_item(),
            Tag::Emphasis => self.push_inline_style(self.styles.emphasis),
            Tag::Strong => self.push_inline_style(self.styles.strong),
            Tag::Strikethrough => self.push_inline_style(self.styles.strikethrough),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => self.push_link(link_type, dest_url.to_string()),
            Tag::FootnoteDefinition(label) => self.start_footnote_definition(&label),
            Tag::DefinitionList => self.start_definition_list(),
            Tag::DefinitionListTitle => self.start_definition_title(),
            Tag::DefinitionListDefinition => self.start_definition_description(),
            Tag::Table(alignments) => self.start_table(alignments),
            Tag::Image { dest_url, .. } => self.start_image(dest_url.to_string()),
            Tag::Superscript | Tag::Subscript => {
                self.push_inline_style(ratatui::style::Style::new().dim().italic());
            }
            Tag::MetadataBlock(kind) => self.start_metadata(kind),
            Tag::HtmlBlock | Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
        }
    }

    pub(super) fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.end_paragraph(),
            TagEnd::Heading(_) => self.end_heading(),
            TagEnd::BlockQuote(_) => self.end_blockquote(),
            TagEnd::CodeBlock => self.end_codeblock(),
            TagEnd::List(_) => self.end_list(),
            TagEnd::Item => {
                self.indent_stack.pop();
                self.pending_marker_line = false;
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => self.pop_inline_style(),
            TagEnd::Link => self.pop_link(),
            TagEnd::FootnoteDefinition => self.end_footnote_definition(),
            TagEnd::DefinitionList => self.needs_newline = true,
            TagEnd::DefinitionListTitle => {
                self.pop_inline_style();
                self.needs_newline = false;
            }
            TagEnd::DefinitionListDefinition => self.end_definition_description(),
            TagEnd::Table => self.end_table(),
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.finish_header();
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    table.finish_row();
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    table.finish_cell();
                }
            }
            TagEnd::Image => self.end_image(),
            TagEnd::MetadataBlock(kind) => self.end_metadata(kind),
            TagEnd::HtmlBlock => {}
        }
    }

    pub(super) fn start_paragraph(&mut self) {
        if self.joined_definition_paragraph {
            self.joined_definition_paragraph = false;
            self.in_paragraph = true;
            return;
        }
        if self.needs_newline {
            self.push_blank_line();
        }
        self.push_line(Line::default());
        self.needs_newline = false;
        self.in_paragraph = true;
    }

    pub(super) fn end_paragraph(&mut self) {
        self.needs_newline = true;
        self.in_paragraph = false;
        self.pending_marker_line = false;
    }

    pub(super) fn start_heading(&mut self, level: HeadingLevel) {
        if self.needs_newline {
            self.push_line(Line::default());
            self.needs_newline = false;
        }
        let heading_style = match level {
            HeadingLevel::H1 => self.styles.h1,
            HeadingLevel::H2 => self.styles.h2,
            HeadingLevel::H3 => self.styles.h3,
            HeadingLevel::H4 => self.styles.h4,
            HeadingLevel::H5 => self.styles.h5,
            HeadingLevel::H6 => self.styles.h6,
        };
        let content = format!("{} ", "#".repeat(level as usize));
        self.push_line(Line::from(vec![Span::styled(
            content,
            self.prose_style.patch(heading_style),
        )]));
        self.push_inline_style(heading_style);
        self.needs_newline = false;
    }

    pub(super) fn end_heading(&mut self) {
        if let Some(suffix) = self.heading_suffix.take() {
            self.push_span(Span::styled(suffix, ratatui::style::Style::new().dim()));
        }
        self.needs_newline = true;
        self.pop_inline_style();
    }

    pub(super) fn start_blockquote(&mut self, kind: Option<BlockQuoteKind>) {
        if self.needs_newline {
            self.push_blank_line();
            self.needs_newline = false;
        }
        self.indent_stack.push(IndentContext::new(
            vec![Span::from("> ")],
            /*marker*/ None,
            /*is_list*/ false,
        ));
        if let Some(kind) = kind {
            let (glyph, label, style) = alert_header_style(kind);
            self.push_line(Line::default());
            self.push_span(Span::styled(format!("{glyph} {label}"), style));
            self.needs_newline = false;
        }
    }

    pub(super) fn end_blockquote(&mut self) {
        self.indent_stack.pop();
        self.needs_newline = true;
    }

    pub(super) fn start_list(&mut self, index: Option<u64>) {
        if self.list_indices.is_empty() && self.needs_newline {
            self.push_line(Line::default());
        }
        self.list_indices.push(index);
    }

    pub(super) fn end_list(&mut self) {
        self.list_indices.pop();
        self.needs_newline = true;
    }

    pub(super) fn start_item(&mut self) {
        self.pending_marker_line = true;
        let depth = self.list_indices.len();
        let width = depth * 4 - 3;
        let marker = if let Some(last_index) = self.list_indices.last_mut() {
            match last_index {
                None => Some(vec![Span::styled(
                    " ".repeat(width - 1) + "- ",
                    self.styles.unordered_list_marker,
                )]),
                Some(index) => {
                    *index += 1;
                    Some(vec![Span::styled(
                        format!("{:width$}. ", *index - 1),
                        self.styles.ordered_list_marker,
                    )])
                }
            }
        } else {
            None
        };
        // Include every digit of an ordered marker (e.g. "12. "), not only
        // the one-digit minimum. Tables and wrapped prose share this prefix.
        let indent_len: usize = marker.iter().flatten().map(Span::width).sum();
        let indent_prefix = vec![Span::from(" ".repeat(indent_len))];
        self.indent_stack.push(IndentContext::new(
            indent_prefix,
            marker,
            /*is_list*/ true,
        ));
        self.needs_newline = false;
    }

    pub(super) fn start_codeblock(&mut self, lang: Option<String>, indent: Option<Span<'static>>) {
        self.flush_current_line();
        if !self.text.lines.is_empty() {
            self.push_blank_line();
        }
        self.in_code_block = true;

        // Extract the language token from the info string.
        let lang = lang
            .as_deref()
            .and_then(|s| s.split([',', ' ', '\t']).next())
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string);
        self.code_block_lang = lang;
        self.code_block_buffer.clear();

        self.indent_stack.push(IndentContext::new(
            vec![indent.unwrap_or_default()],
            /*marker*/ None,
            /*is_list*/ false,
        ));
        self.needs_newline = true;
    }

    pub(super) fn end_codeblock(&mut self) {
        if let Some(lang) = self.code_block_lang.take() {
            let code = std::mem::take(&mut self.code_block_buffer);
            self.flush_current_line();
            let start = self.text.lines.len();
            if !code.is_empty() {
                let highlighted = highlight_code_to_lines(&code, &lang);
                for hl_line in highlighted {
                    self.push_line(Line::default());
                    for span in hl_line.spans {
                        self.push_span(span);
                    }
                }
            }
            // Containers keep their literal fallback: replacing a list/quote with
            // an image would otherwise discard its indentation and markers.
            if lang.eq_ignore_ascii_case("mermaid")
                && self.indent_stack.len() == 1
                && crate::mermaid::within_source_limits(&code)
            {
                self.flush_current_line();
                if self.text.lines.len() > start {
                    self.mermaid_blocks
                        .push((start..self.text.lines.len(), code));
                }
            }
        }

        self.needs_newline = true;
        self.in_code_block = false;
        self.indent_stack.pop();
    }
}
