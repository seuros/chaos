use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};
use tui_input::backend::crossterm::to_input_request;
use tui_input::{Input, InputRequest};

#[derive(Default)]
pub(super) struct SingleLineInput(Input);

impl From<String> for SingleLineInput {
    fn from(value: String) -> Self {
        Self(Input::new(value))
    }
}

impl SingleLineInput {
    pub(super) fn text(&self) -> &str {
        self.0.value()
    }

    pub(super) fn insert_str(&mut self, text: &str) -> bool {
        if text.chars().any(char::is_control) {
            return false;
        }
        let cursor = self.0.cursor();
        let mut value = self.text().to_owned();
        let byte = value
            .char_indices()
            .nth(cursor)
            .map_or(value.len(), |(i, _)| i);
        value.insert_str(byte, text);
        self.0 = std::mem::take(&mut self.0).with_value(value);
        self.0
            .handle(InputRequest::SetCursor(cursor + text.chars().count()));
        true
    }

    pub(super) fn input(&mut self, event: KeyEvent) {
        if event.kind == KeyEventKind::Release
            || matches!(event.code, KeyCode::Char(c) if c.is_control())
        {
            return;
        }
        use InputRequest::*;
        let request = match (event.code, event.modifiers) {
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => Some(DeleteFromStart),
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => Some(DeleteNextChar),
            (KeyCode::Char('h'), modifiers)
                if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT =>
            {
                Some(DeletePrevWord)
            }
            (KeyCode::Left | KeyCode::Char('b'), KeyModifiers::ALT | KeyModifiers::META) => {
                Some(GoToPrevWord)
            }
            (KeyCode::Right | KeyCode::Char('f'), KeyModifiers::ALT | KeyModifiers::META) => {
                Some(GoToNextWord)
            }
            (KeyCode::Delete | KeyCode::Char('d'), KeyModifiers::ALT | KeyModifiers::META) => {
                Some(DeleteNextWord)
            }
            (_, modifiers)
                if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT
                    && !crate::key_hint::is_altgr(modifiers) =>
            {
                None
            }
            _ => to_input_request(&Event::Key(event)),
        };
        if let Some(request) = request {
            self.0.handle(request);
        }
    }

    fn scroll(&self, width: u16) -> u16 {
        self.0
            .visual_scroll(usize::from(width.saturating_sub(1)))
            .min(usize::from(u16::MAX)) as u16
    }

    pub(super) fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let x = self
            .0
            .visual_cursor()
            .saturating_sub(usize::from(self.scroll(area.width)));
        (!area.is_empty() && x < usize::from(area.width)).then(|| (area.x + x as u16, area.y))
    }
}

impl Widget for &SingleLineInput {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.text())
            .scroll((0, self.scroll(area.width)))
            .render(area, buf);
    }
}
