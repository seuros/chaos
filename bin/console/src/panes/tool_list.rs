//! Tool list pane — shows all tools visible to the model.

use crate::theme;
use crate::tool_badges::tool_name_style;
use crate::tool_badges::tool_name_style_from_labels_and_annotations;
use chaos_ipc::protocol::ToolSummary;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use ratatui_hypertile::KeyChord;
use ratatui_hypertile::KeyCode as HypertileKeyCode;
use ratatui_hypertile::Modifiers;
use ratatui_hypertile_extras::keychord_from_crossterm;

/// Result of a key event handled by the tool-list pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolListKeyResult {
    /// The pane consumed the key internally (e.g. scroll).
    Consumed,
    /// The pane wants to be closed (Esc pressed).
    Close,
    /// The key was not handled — let the parent deal with it.
    Ignored,
}

/// State for the tool-list tile.
pub(crate) struct ToolListPane {
    tools: Vec<ToolSummary>,
    scroll: u16,
}

impl ToolListPane {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            scroll: 0,
        }
    }

    /// Replace the tool list contents.
    ///
    /// Sorts by source so the renderer can group tools under a single
    /// `[server]` header per MCP server.
    pub fn set_tools(&mut self, mut tools: Vec<ToolSummary>) {
        tools.sort_by(|a, b| a.source.cmp(&b.source));
        self.tools = tools;
        self.scroll = 0;
    }

    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }

    /// Handle a key event when this pane is focused.
    #[allow(dead_code)]
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> ToolListKeyResult {
        if key_event.kind != KeyEventKind::Press && key_event.kind != KeyEventKind::Repeat {
            return ToolListKeyResult::Ignored;
        }

        let Some(chord) = keychord_from_crossterm(key_event) else {
            return ToolListKeyResult::Ignored;
        };

        self.handle_chord(chord)
    }

    pub fn handle_chord(&mut self, chord: KeyChord) -> ToolListKeyResult {
        // Ctrl+C is never consumed — let the app handle quit.
        if chord.modifiers.contains(Modifiers::CTRL)
            && matches!(chord.code, HypertileKeyCode::Char('c'))
        {
            return ToolListKeyResult::Ignored;
        }

        match chord.code {
            HypertileKeyCode::Escape | HypertileKeyCode::Char('q') => ToolListKeyResult::Close,
            HypertileKeyCode::Up | HypertileKeyCode::Char('k') => {
                self.scroll_up(1);
                ToolListKeyResult::Consumed
            }
            HypertileKeyCode::Down | HypertileKeyCode::Char('j') => {
                self.scroll_down(1);
                ToolListKeyResult::Consumed
            }
            HypertileKeyCode::PageUp => {
                self.scroll_up(10);
                ToolListKeyResult::Consumed
            }
            HypertileKeyCode::PageDown => {
                self.scroll_down(10);
                ToolListKeyResult::Consumed
            }
            HypertileKeyCode::Home => {
                self.scroll = 0;
                ToolListKeyResult::Consumed
            }
            HypertileKeyCode::End => {
                // Scroll to a large value; render will clamp.
                self.scroll = u16::MAX;
                ToolListKeyResult::Consumed
            }
            _ => ToolListKeyResult::Ignored,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, is_focused: bool) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Tools ")
            .border_style(if is_focused {
                theme::highlight()
            } else {
                theme::border()
            });

        let inner = block.inner(area);
        block.render(area, buf);

        if self.tools.is_empty() {
            let loading = Paragraph::new("Loading tools...").style(theme::dim());
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
                lines.push(Line::from(vec![Span::styled(
                    format!("  [{}]", tool.source),
                    Style::default()
                        .fg(theme::cyan())
                        .add_modifier(Modifier::BOLD),
                )]));
                current_source = Some(tool.source.clone());
            }

            let name_style = if tool.annotation_labels.is_empty() && tool.annotations.is_none() {
                tool_name_style()
            } else {
                tool_name_style_from_labels_and_annotations(
                    &tool.annotation_labels,
                    tool.annotations.as_ref(),
                )
            };
            let mut spans = vec![Span::styled(format!("    {}", tool.name), name_style)];
            if !tool.description.is_empty() {
                spans.push(Span::styled(
                    format!(" — {}", tool.description),
                    theme::dim(),
                ));
            }
            lines.push(Line::from(spans));
        }

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        paragraph.render(inner, buf);
    }
}
