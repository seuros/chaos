//! Overlay UIs rendered in an alternate screen.
//!
//! This module implements the pager-style overlays used by the TUI, including the transcript
//! overlay (`Ctrl+T`) that renders a full history view separate from the main viewport.
//!
//! The transcript overlay renders committed transcript cells plus an optional render-only live tail
//! derived from the current in-flight active cell. Because rebuilding wrapped `Line`s on every draw
//! can be expensive, that live tail is cached and only recomputed when its cache key changes, which
//! is derived from the terminal width (wrapping), an active-cell revision (in-place mutations), the
//! stream-continuation flag (spacing), and an animation tick (time-based spinner/shimmer output).
//!
//! The transcript overlay live tail is kept in sync by `App` during draws: `App` supplies an
//! `ActiveCellTranscriptKey` and a function to compute the active cell transcript lines, and
//! `TranscriptOverlay::sync_live_tail` uses the key to decide when the cached tail must be
//! recomputed. `ChatWidget` is responsible for producing a key that changes when the active cell
//! mutates in place or when its transcript output is time-dependent.

mod pager_view;
mod settings_overlay;
mod static_overlay;
mod transcript_overlay;

pub(crate) use settings_overlay::SettingsOverlay;
pub(crate) use static_overlay::{AccountsOverlay, StaticOverlay};
pub(crate) use transcript_overlay::TranscriptOverlay;

use std::io::Result;

use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::onboarding::auth::AccountsCompletion;
use crate::onboarding::auth::AccountsWidget;
use crate::render::renderable::Renderable;
use crate::tui;
use crate::tui::TuiEvent;
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

pub(crate) enum Overlay {
    Transcript(TranscriptOverlay),
    Static(StaticOverlay),
    Accounts(AccountsOverlay),
    Settings(SettingsOverlay),
}

impl Overlay {
    pub(crate) fn new_transcript(
        cells: Vec<std::sync::Arc<dyn crate::history_cell::HistoryCell>>,
    ) -> Self {
        Self::Transcript(TranscriptOverlay::new(cells))
    }

    pub(crate) fn new_static_with_lines(lines: Vec<Line<'static>>, title: String) -> Self {
        Self::Static(StaticOverlay::with_title(lines, title))
    }

    pub(crate) fn new_static_with_renderables(
        renderables: Vec<Box<dyn Renderable>>,
        title: String,
    ) -> Self {
        Self::Static(StaticOverlay::with_renderables(renderables, title))
    }

    pub(crate) fn new_accounts(widget: AccountsWidget) -> Self {
        Self::Accounts(AccountsOverlay::new(widget))
    }

    pub(crate) fn new_settings(
        title: &'static str,
        view: Box<dyn libui::bottom_pane::BottomPaneView>,
    ) -> Self {
        Self::Settings(SettingsOverlay::new(title, view))
    }

    pub(crate) fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match self {
            Overlay::Transcript(o) => o.handle_event(tui, event),
            Overlay::Static(o) => o.handle_event(tui, event),
            Overlay::Accounts(o) => o.handle_event(tui, event),
            Overlay::Settings(o) => o.handle_event(tui, event),
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        match self {
            Overlay::Transcript(o) => o.is_done(),
            Overlay::Static(o) => o.is_done(),
            Overlay::Accounts(o) => o.is_done(),
            Overlay::Settings(o) => o.is_done(),
        }
    }

    pub(crate) fn auth_completion(&self) -> Option<AccountsCompletion> {
        match self {
            Overlay::Accounts(o) => o.completion(),
            _ => None,
        }
    }

    pub(crate) fn is_transcript(&self) -> bool {
        matches!(self, Overlay::Transcript(_))
    }

    /// Returns true for overlays that handle `TuiEvent::Mouse` and need mouse capture
    /// to receive wheel events (e.g. in Zellij where the normal buffer is used).
    pub(crate) fn wants_mouse_capture(&self) -> bool {
        matches!(self, Overlay::Transcript(_) | Overlay::Static(_))
    }
}

/// Shared rendering for the scrollable pager overlays (static + transcript):
/// a [`PagerView`](pager_view::PagerView) fills the top region and a two-line
/// hint bar sits in the bottom three rows. Implementors supply access to the
/// view and the hint bar; `render` lays them out identically for both.
pub(crate) trait PagerOverlay {
    fn view(&mut self) -> &mut pager_view::PagerView;
    fn render_hints(&self, area: Rect, buf: &mut Buffer);

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let top_h = area.height.saturating_sub(PAGER_HINT_ROWS);
        let top = Rect::new(area.x, area.y, area.width, top_h);
        let bottom = Rect::new(area.x, area.y + top_h, area.width, PAGER_HINT_ROWS);
        self.view().render(top, buf);
        self.render_hints(bottom, buf);
    }
}

const PAGER_HINT_ROWS: u16 = 3;

/// The pager's area before (or after) its first draw. An inline overlay may
/// still have the small composer viewport when its opening wheel event arrives.
fn pager_area(tui: &tui::Tui) -> Rect {
    let size = tui.terminal.last_known_screen_size;
    let reserved = if tui.is_alt_screen_active() {
        0
    } else {
        tui.top_reserved_rows().min(size.height)
    };
    Rect::new(
        0,
        reserved,
        size.width,
        size.height
            .saturating_sub(reserved)
            .saturating_sub(PAGER_HINT_ROWS),
    )
}

pub(crate) const KEY_UP: KeyBinding = key_hint::plain(KeyCode::Up);
pub(crate) const KEY_DOWN: KeyBinding = key_hint::plain(KeyCode::Down);
pub(crate) const KEY_K: KeyBinding = key_hint::plain(KeyCode::Char('k'));
pub(crate) const KEY_J: KeyBinding = key_hint::plain(KeyCode::Char('j'));
pub(crate) const KEY_PAGE_UP: KeyBinding = key_hint::plain(KeyCode::PageUp);
pub(crate) const KEY_PAGE_DOWN: KeyBinding = key_hint::plain(KeyCode::PageDown);
pub(crate) const KEY_SPACE: KeyBinding = key_hint::plain(KeyCode::Char(' '));
pub(crate) const KEY_SHIFT_SPACE: KeyBinding = key_hint::shift(KeyCode::Char(' '));
pub(crate) const KEY_HOME: KeyBinding = key_hint::plain(KeyCode::Home);
pub(crate) const KEY_END: KeyBinding = key_hint::plain(KeyCode::End);
pub(crate) const KEY_LEFT: KeyBinding = key_hint::plain(KeyCode::Left);
pub(crate) const KEY_RIGHT: KeyBinding = key_hint::plain(KeyCode::Right);
pub(crate) const KEY_CTRL_F: KeyBinding = key_hint::ctrl(KeyCode::Char('f'));
pub(crate) const KEY_CTRL_D: KeyBinding = key_hint::ctrl(KeyCode::Char('d'));
pub(crate) const KEY_CTRL_B: KeyBinding = key_hint::ctrl(KeyCode::Char('b'));
pub(crate) const KEY_CTRL_U: KeyBinding = key_hint::ctrl(KeyCode::Char('u'));
pub(crate) const KEY_Q: KeyBinding = key_hint::plain(KeyCode::Char('q'));
pub(crate) const KEY_ESC: KeyBinding = key_hint::plain(KeyCode::Esc);
pub(crate) const KEY_ENTER: KeyBinding = key_hint::plain(KeyCode::Enter);
pub(crate) const KEY_CTRL_T: KeyBinding = key_hint::ctrl(KeyCode::Char('t'));
pub(crate) const KEY_CTRL_C: KeyBinding = key_hint::ctrl(KeyCode::Char('c'));

// Common pager navigation hints rendered on the first line
pub(crate) const PAGER_KEY_HINTS: &[(&[KeyBinding], &str)] = &[
    (&[KEY_UP, KEY_DOWN], "to scroll"),
    (&[KEY_PAGE_UP, KEY_PAGE_DOWN], "to page"),
    (&[KEY_HOME, KEY_END], "to jump"),
];

pub(crate) fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width).max(1);
    let height = height.min(area.height).max(1);
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height) / 2);
    Rect::new(x, y, width, height)
}

// Render a single line of key hints from (key(s), description) pairs.
pub(crate) fn render_key_hints(area: Rect, buf: &mut Buffer, pairs: &[(&[KeyBinding], &str)]) {
    let mut spans: Vec<Span<'static>> = vec![" ".into()];
    let mut first = true;
    for (keys, desc) in pairs {
        if !first {
            spans.push("   ".into());
        }
        for (i, key) in keys.iter().enumerate() {
            if i > 0 {
                spans.push("/".into());
            }
            spans.push(Span::from(key));
        }
        spans.push(" ".into());
        spans.push(Span::from(desc.to_string()));
        first = false;
    }
    Paragraph::new(vec![Line::from(spans).dim()]).render(area, buf);
}

pub(crate) fn render_offset_content(
    area: Rect,
    buf: &mut Buffer,
    renderable: &dyn Renderable,
    scroll_offset: u16,
) -> u16 {
    let height = renderable.desired_height(area.width);
    let mut tall_buf = ratatui::buffer::Buffer::empty(Rect::new(
        0,
        0,
        area.width,
        height.min(area.height + scroll_offset),
    ));
    renderable.render(*tall_buf.area(), &mut tall_buf);
    let copy_height = area
        .height
        .min(tall_buf.area().height.saturating_sub(scroll_offset));
    for y in 0..copy_height {
        let src_y = y + scroll_offset;
        for x in 0..area.width {
            buf[(area.x + x, area.y + y)] = tall_buf[(x, src_y)].clone();
        }
    }

    copy_height
}

#[cfg(test)]
pub(crate) mod tests;
