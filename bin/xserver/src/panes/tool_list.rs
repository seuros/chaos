//! Tool list pane — shows all tools visible to the model.

use chaos_ipc::protocol::ToolSummary;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// State for the tool-list tile.
#[allow(dead_code)]
pub(crate) struct ToolListPane {
    tools: Vec<ToolSummary>,
    scroll: u16,
}

#[allow(dead_code)]
impl ToolListPane {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            scroll: 0,
        }
    }

    /// Replace the tool list contents.
    pub fn set_tools(&mut self, tools: Vec<ToolSummary>) {
        self.tools = tools;
        self.scroll = 0;
    }

    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Tools ")
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        block.render(area, buf);

        if self.tools.is_empty() {
            let loading = Paragraph::new("Loading tools...")
                .style(Style::default().fg(Color::DarkGray));
            loading.render(inner, buf);
            return;
        }

        // Group tools by source.
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut current_source: Option<String> = None;

        for tool in &self.tools {
            if current_source.as_ref() != Some(&tool.source) {
                if current_source.is_some() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  [{}]", tool.source),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                current_source = Some(tool.source.clone());
            }

            let name_span = Span::styled(
                format!("    {}", tool.name),
                Style::default().bold(),
            );
            let desc_span = if tool.description.is_empty() {
                Span::raw("")
            } else {
                Span::styled(
                    format!(" — {}", tool.description),
                    Style::default().fg(Color::DarkGray),
                )
            };
            lines.push(Line::from(vec![name_span, desc_span]));
        }

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        paragraph.render(inner, buf);
    }
}
