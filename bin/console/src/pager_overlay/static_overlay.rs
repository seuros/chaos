use std::io::Result;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::text::Text;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget, WidgetRef, Wrap};

use crate::key_hint::KeyBinding;
use crate::onboarding::auth::{AuthCompletion, AuthModeWidget};
use crate::onboarding::onboarding_screen::KeyboardHandler;
use crate::render::renderable::Renderable;
use crate::tui::{self, TuiEvent};

use super::pager_view::PagerView;
use super::transcript_overlay::CachedRenderable;
use super::{KEY_CTRL_C, KEY_Q, PAGER_KEY_HINTS, centered_rect, render_key_hints};

pub(crate) struct StaticOverlay {
    pub(super) view: PagerView,
    is_done: bool,
}

impl StaticOverlay {
    pub(crate) fn with_title(lines: Vec<Line<'static>>, title: String) -> Self {
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        Self::with_renderables(vec![Box::new(CachedRenderable::new(paragraph))], title)
    }

    pub(crate) fn with_renderables(renderables: Vec<Box<dyn Renderable>>, title: String) -> Self {
        Self {
            view: PagerView::new(renderables, title, /*scroll_offset*/ 0),
            is_done: false,
        }
    }

    fn render_hints(&self, area: Rect, buf: &mut Buffer) {
        let line1 = Rect::new(area.x, area.y, area.width, 1);
        let line2 = Rect::new(area.x, area.y.saturating_add(1), area.width, 1);
        render_key_hints(line1, buf, PAGER_KEY_HINTS);
        let pairs: Vec<(&[KeyBinding], &str)> = vec![(&[KEY_Q], "to quit")];
        render_key_hints(line2, buf, &pairs);
    }

    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let top_h = area.height.saturating_sub(3);
        let top = Rect::new(area.x, area.y, area.width, top_h);
        let bottom = Rect::new(area.x, area.y + top_h, area.width, 3);
        self.view.render(top, buf);
        self.render_hints(bottom, buf);
    }

    pub(crate) fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match event {
            TuiEvent::Key(key_event) => match key_event {
                e if KEY_Q.is_press(e) || KEY_CTRL_C.is_press(e) => {
                    self.is_done = true;
                    Ok(())
                }
                other => self.view.handle_key_event(tui, other),
            },
            TuiEvent::Mouse(mouse_event) => self.view.handle_mouse_event(tui, mouse_event),
            TuiEvent::Draw => {
                tui.draw(u16::MAX, |frame| {
                    self.render(frame.area(), frame.buffer);
                })?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
    }
}

pub(crate) struct AuthOverlay {
    widget: AuthModeWidget,
    done: bool,
    completion: Option<AuthCompletion>,
}

impl AuthOverlay {
    pub(super) fn new(widget: AuthModeWidget) -> Self {
        Self {
            widget,
            done: false,
            completion: None,
        }
    }

    pub(super) fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match event {
            TuiEvent::Key(key_event)
                if matches!(
                    key_event.kind,
                    crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat
                ) =>
            {
                if key_event.code == crossterm::event::KeyCode::Esc && self.widget.can_exit_popup()
                {
                    self.done = true;
                } else {
                    self.widget.handle_key_event(key_event);
                    if let Some(completion) = self.widget.completion() {
                        self.completion = Some(completion);
                        self.done = true;
                    }
                }
            }
            TuiEvent::Paste(pasted) => {
                self.widget.handle_paste(pasted);
                if let Some(completion) = self.widget.completion() {
                    self.completion = Some(completion);
                    self.done = true;
                }
            }
            TuiEvent::Draw => {
                tui.draw(u16::MAX, |frame| {
                    let area = frame.area();
                    Clear.render(area, frame.buffer);

                    let popup = centered_rect(
                        area,
                        area.width.saturating_sub(4).clamp(56, 88),
                        area.height.saturating_sub(4).clamp(14, 24),
                    );
                    let inner = popup.inner(Margin {
                        horizontal: 2,
                        vertical: 1,
                    });

                    let block = Block::default()
                        .title(Line::from(vec![
                            "/".dim(),
                            " login".fg(crate::theme::cyan()),
                        ]))
                        .title_alignment(Alignment::Left)
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray));
                    block.render(popup, frame.buffer);
                    self.widget.render_ref(inner, frame.buffer);
                })?;
            }
            TuiEvent::Mouse(_) => {}
            TuiEvent::Key(_) => {}
        }
        Ok(())
    }

    pub(super) fn is_done(&self) -> bool {
        self.done
    }

    pub(super) fn completion(&self) -> Option<AuthCompletion> {
        self.completion
    }
}
