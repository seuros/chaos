use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

use crate::onboarding::onboarding_screen::KeyboardHandler;
use crate::onboarding::onboarding_screen::StepStateProvider;

use super::onboarding_screen::StepState;

pub(crate) struct WelcomeWidget {
    pub is_logged_in: bool,
}

impl KeyboardHandler for WelcomeWidget {
    fn handle_key_event(&mut self, _key_event: KeyEvent) {}
}

impl WelcomeWidget {
    pub(crate) fn new(is_logged_in: bool) -> Self {
        Self { is_logged_in }
    }
}

impl Widget for &WelcomeWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let lines: Vec<Line> = vec![
            Line::from(vec![
                "  ".into(),
                "Welcome to ".into(),
                "Chaos".bold(),
                " — AI harness for operators who read before they run.".into(),
            ]),
            "".into(),
            Line::from(vec!["  Default tools: ".into(), "none destructive.".bold()]),
            Line::from(vec![
                "  Default permissions: ".into(),
                "none granted.".bold(),
            ]),
            Line::from(vec![
                "  Mistakes: ".into(),
                "yours, not the model's.".bold(),
            ]),
            "".into(),
            Line::from(vec!["  ".into(), "You were warned.".dim()]),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

impl StepStateProvider for WelcomeWidget {
    fn get_step_state(&self) -> StepState {
        match self.is_logged_in {
            true => StepState::Hidden,
            false => StepState::Complete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn row_containing(buf: &Buffer, needle: &str) -> Option<u16> {
        (0..buf.area.height).find(|&y| {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            row.contains(needle)
        })
    }

    #[test]
    fn welcome_renders_text_at_top() {
        let widget = WelcomeWidget::new(false);
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        (&widget).render(area, &mut buf);

        let welcome_row = row_containing(&buf, "Welcome");
        assert_eq!(welcome_row, Some(0));
    }
}
