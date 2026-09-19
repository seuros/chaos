use std::io::Result;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::text::Text;
use ratatui::widgets::{Paragraph, WidgetRef, Wrap};

use crate::key_hint::KeyBinding;
use crate::onboarding::auth::{AccountsCompletion, AccountsWidget};
use crate::onboarding::onboarding_screen::KeyboardHandler;
use crate::render::renderable::Renderable;
use crate::tui::{self, TuiEvent};

use super::pager_view::PagerView;
use super::settings_overlay::render_settings_panel;
use super::transcript_overlay::CachedRenderable;
use super::{KEY_CTRL_C, KEY_Q, PAGER_KEY_HINTS, PagerOverlay, render_key_hints};

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

impl PagerOverlay for StaticOverlay {
    fn view(&mut self) -> &mut PagerView {
        &mut self.view
    }

    fn render_hints(&self, area: Rect, buf: &mut Buffer) {
        let line1 = Rect::new(area.x, area.y, area.width, 1);
        let line2 = Rect::new(area.x, area.y.saturating_add(1), area.width, 1);
        render_key_hints(line1, buf, PAGER_KEY_HINTS);
        let pairs: Vec<(&[KeyBinding], &str)> = vec![(&[KEY_Q], "to quit")];
        render_key_hints(line2, buf, &pairs);
    }
}

pub(crate) struct AccountsOverlay {
    widget: AccountsWidget,
    done: bool,
    completion: Option<AccountsCompletion>,
}

impl AccountsOverlay {
    pub(super) fn new(widget: AccountsWidget) -> Self {
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
                let was_escape = key_event.code == crossterm::event::KeyCode::Esc;
                let should_close_before_event = was_escape && self.widget.should_close_on_escape();
                self.widget.handle_key_event(key_event);
                if should_close_before_event && self.widget.should_close_on_escape() {
                    self.done = true;
                    return Ok(());
                }
                if let Some(completion) = self.widget.completion() {
                    self.completion = Some(completion);
                    self.done = true;
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
                    let inner = render_settings_panel(frame.area(), frame.buffer, "accounts");
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

    pub(super) fn completion(&self) -> Option<AccountsCompletion> {
        self.completion.clone()
    }
}
