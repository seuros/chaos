use std::io::Result;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use libui::bottom_pane::BottomPaneView;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Widget};

use super::KEY_CTRL_C;
use crate::tui::{self, TuiEvent};

pub(crate) struct SettingsOverlay {
    pub(crate) title: &'static str,
    views: Vec<Box<dyn BottomPaneView>>,
    suppressed_repeat_key: Option<KeyCode>,
}

impl SettingsOverlay {
    pub(super) fn new(title: &'static str, view: Box<dyn BottomPaneView>) -> Self {
        Self {
            title,
            views: vec![view],
            suppressed_repeat_key: None,
        }
    }

    pub(crate) fn open_form(&mut self, view: Box<dyn BottomPaneView>) {
        if self.views.len() == 1 && !self.is_done() {
            self.views.push(view);
            self.suppressed_repeat_key = Some(KeyCode::Enter);
        }
    }

    fn handle_key_event(&mut self, event: KeyEvent) {
        if event.kind == KeyEventKind::Release
            || (event.kind == KeyEventKind::Repeat
                && self.suppressed_repeat_key == Some(event.code))
        {
            return;
        }
        self.suppressed_repeat_key = None;
        let nested = self.views.len() > 1;
        let Some(view) = self.views.last_mut() else {
            return;
        };
        if KEY_CTRL_C.is_press(event) || event.code == KeyCode::Esc {
            if event.code == KeyCode::Esc && view.prefer_esc_to_handle_key_event() {
                view.handle_key_event(event);
            } else {
                view.on_ctrl_c();
            }
            if nested && view.is_complete() {
                self.views.pop();
                self.suppressed_repeat_key = Some(event.code);
            }
        } else {
            view.handle_key_event(event);
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        if let Some(view) = self.views.last_mut() {
            view.handle_paste(pasted);
        }
    }

    pub(super) fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match event {
            TuiEvent::Key(event) => {
                self.handle_key_event(event);
                tui.frame_requester().schedule_frame();
            }
            TuiEvent::Paste(pasted) => {
                self.handle_paste(pasted);
                tui.frame_requester().schedule_frame();
            }
            TuiEvent::Draw => {
                tui.draw(u16::MAX, |frame| self.render(frame.area(), frame.buffer))?;
            }
            TuiEvent::Mouse(_) => {}
        }
        Ok(())
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let inner = render_settings_panel(area, buf, self.title);
        if let Some(view) = self.views.last() {
            view.render(inner, buf);
        }
    }

    pub(super) fn is_done(&self) -> bool {
        self.views.last().is_none_or(|view| view.is_complete())
    }
}

pub(super) fn render_settings_panel(area: Rect, buf: &mut Buffer, title: &str) -> Rect {
    if area.is_empty() {
        return area;
    }
    Clear.render(area, buf);
    let popup = area.centered(
        Constraint::Length(area.width.saturating_sub(4).clamp(56, 88)),
        Constraint::Length(area.height.saturating_sub(4).clamp(14, 24)),
    );
    let block = Block::default()
        .title(Line::from(vec![
            "/".dim(),
            format!(" {title}").fg(crate::theme::accent_color()),
        ]))
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(popup);
    block.render(popup, buf);
    inner
}

#[cfg(test)]
mod tests;
