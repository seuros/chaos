use std::io::Result;

use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Span;
use ratatui::widgets::{Clear, Widget};

use crate::render::renderable::Renderable;
use crate::tui;

use super::{
    KEY_CTRL_B, KEY_CTRL_D, KEY_CTRL_F, KEY_CTRL_U, KEY_DOWN, KEY_END, KEY_HOME, KEY_J, KEY_K,
    KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_SHIFT_SPACE, KEY_SPACE, KEY_UP, render_offset_content,
};

/// Generic widget for rendering a pager view.
pub(super) struct PagerView {
    pub(super) renderables: Vec<Box<dyn Renderable>>,
    pub(super) scroll_offset: usize,
    pub(super) title: String,
    pub(super) last_content_height: Option<usize>,
    pub(super) last_rendered_height: Option<usize>,
    /// If set, on next render ensure this chunk is visible.
    pub(super) pending_scroll_chunk: Option<usize>,
}

impl PagerView {
    pub(super) fn new(
        renderables: Vec<Box<dyn Renderable>>,
        title: String,
        scroll_offset: usize,
    ) -> Self {
        Self {
            renderables,
            scroll_offset,
            title,
            last_content_height: None,
            last_rendered_height: None,
            pending_scroll_chunk: None,
        }
    }

    pub(super) fn content_height(&self, width: u16) -> usize {
        self.renderables
            .iter()
            .map(|c| c.desired_height(width) as usize)
            .sum()
    }

    pub(super) fn resolved_scroll_offset_for_area(&self, content_area: Rect) -> usize {
        let max_scroll = self
            .content_height(content_area.width)
            .saturating_sub(content_area.height as usize);
        if self.scroll_offset == usize::MAX {
            max_scroll
        } else {
            self.scroll_offset.min(max_scroll)
        }
    }

    pub(super) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        self.render_header(area, buf);
        let content_area = self.content_area(area);
        self.update_last_content_height(content_area.height);
        let content_height = self.content_height(content_area.width);
        self.last_rendered_height = Some(content_height);
        if let Some(idx) = self.pending_scroll_chunk.take() {
            self.ensure_chunk_visible(idx, content_area);
        }
        self.scroll_offset = self
            .scroll_offset
            .min(content_height.saturating_sub(content_area.height as usize));

        self.render_content(content_area, buf);

        self.render_bottom_bar(area, content_area, buf, content_height);
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        Span::from("/ ".repeat(area.width as usize / 2))
            .dim()
            .render(area, buf);
        let header = format!("/ {}", self.title);
        header.dim().render(area, buf);
    }

    fn render_content(&self, area: Rect, buf: &mut Buffer) {
        let mut y = -(self.scroll_offset as isize);
        let mut drawn_bottom = area.y;
        for renderable in &self.renderables {
            let top = y;
            let height = renderable.desired_height(area.width) as isize;
            y += height;
            let bottom = y;
            if bottom < area.y as isize {
                continue;
            }
            if top > area.y as isize + area.height as isize {
                break;
            }
            if top < 0 {
                let drawn = render_offset_content(area, buf, &**renderable, (-top) as u16);
                drawn_bottom = drawn_bottom.max(area.y + drawn);
            } else {
                let draw_height = (height as u16).min(area.height.saturating_sub(top as u16));
                let draw_area = Rect::new(area.x, area.y + top as u16, area.width, draw_height);
                renderable.render(draw_area, buf);
                drawn_bottom = drawn_bottom.max(draw_area.y.saturating_add(draw_area.height));
            }
        }

        for y in drawn_bottom..area.bottom() {
            if area.width == 0 {
                break;
            }
            buf[(area.x, y)] = Cell::from('~');
            for x in area.x + 1..area.right() {
                buf[(x, y)] = Cell::from(' ');
            }
        }
    }

    fn render_bottom_bar(
        &self,
        full_area: Rect,
        content_area: Rect,
        buf: &mut Buffer,
        total_len: usize,
    ) {
        let sep_y = content_area.bottom();
        let sep_rect = Rect::new(full_area.x, sep_y, full_area.width, 1);

        Span::from("─".repeat(sep_rect.width as usize))
            .dim()
            .render(sep_rect, buf);
        let percent = if total_len == 0 {
            100
        } else {
            let max_scroll = total_len.saturating_sub(content_area.height as usize);
            if max_scroll == 0 {
                100
            } else {
                (((self.scroll_offset.min(max_scroll)) as f32 / max_scroll as f32) * 100.0).round()
                    as u8
            }
        };
        let pct_text = format!(" {percent}% ");
        let pct_w = pct_text.chars().count() as u16;
        let pct_x = sep_rect.x + sep_rect.width - pct_w - 1;
        Span::from(pct_text)
            .dim()
            .render(Rect::new(pct_x, sep_rect.y, pct_w, 1), buf);
    }

    pub(super) fn handle_key_event(
        &mut self,
        tui: &mut tui::Tui,
        key_event: KeyEvent,
    ) -> Result<()> {
        let content_area = self.content_area(tui.terminal.viewport_area);
        let current_offset = self.resolved_scroll_offset_for_area(content_area);
        match key_event {
            e if KEY_UP.is_press(e) || KEY_K.is_press(e) => {
                self.scroll_offset = current_offset.saturating_sub(1);
            }
            e if KEY_DOWN.is_press(e) || KEY_J.is_press(e) => {
                self.scroll_offset = current_offset.saturating_add(1);
            }
            e if KEY_PAGE_UP.is_press(e)
                || KEY_SHIFT_SPACE.is_press(e)
                || KEY_CTRL_B.is_press(e) =>
            {
                let page_height = self.page_height(tui.terminal.viewport_area);
                self.scroll_offset = current_offset.saturating_sub(page_height);
            }
            e if KEY_PAGE_DOWN.is_press(e) || KEY_SPACE.is_press(e) || KEY_CTRL_F.is_press(e) => {
                let page_height = self.page_height(tui.terminal.viewport_area);
                self.scroll_offset = current_offset.saturating_add(page_height);
            }
            e if KEY_CTRL_D.is_press(e) => {
                let half_page = (content_area.height as usize).saturating_add(1) / 2;
                self.scroll_offset = current_offset.saturating_add(half_page);
            }
            e if KEY_CTRL_U.is_press(e) => {
                let half_page = (content_area.height as usize).saturating_add(1) / 2;
                self.scroll_offset = current_offset.saturating_sub(half_page);
            }
            e if KEY_HOME.is_press(e) => {
                self.scroll_offset = 0;
            }
            e if KEY_END.is_press(e) => {
                self.scroll_offset = usize::MAX;
            }
            _ => {
                return Ok(());
            }
        }
        tui.frame_requester()
            .schedule_frame_in(crate::tui::TARGET_FRAME_INTERVAL);
        Ok(())
    }

    pub(super) fn handle_mouse_event(
        &mut self,
        tui: &mut tui::Tui,
        mouse_event: MouseEvent,
    ) -> Result<()> {
        if !self.apply_mouse_scroll(mouse_event.kind, tui.terminal.viewport_area) {
            return Ok(());
        }
        tui.frame_requester()
            .schedule_frame_in(crate::tui::TARGET_FRAME_INTERVAL);
        Ok(())
    }

    pub(super) fn apply_mouse_scroll(&mut self, kind: MouseEventKind, viewport_area: Rect) -> bool {
        let content_area = self.content_area(viewport_area);
        let current_offset = self.resolved_scroll_offset_for_area(content_area);
        match kind {
            MouseEventKind::ScrollUp => {
                self.scroll_offset = current_offset.saturating_sub(3);
                true
            }
            MouseEventKind::ScrollDown => {
                self.scroll_offset = current_offset.saturating_add(3);
                true
            }
            _ => false,
        }
    }

    /// Returns the height of one page in content rows.
    pub(super) fn page_height(&self, viewport_area: Rect) -> usize {
        self.last_content_height
            .unwrap_or_else(|| self.content_area(viewport_area).height as usize)
    }

    pub(super) fn update_last_content_height(&mut self, height: u16) {
        self.last_content_height = Some(height as usize);
    }

    pub(super) fn content_area(&self, area: Rect) -> Rect {
        let mut area = area;
        area.y = area.y.saturating_add(1);
        area.height = area.height.saturating_sub(2);
        area
    }

    pub(super) fn is_scrolled_to_bottom(&self) -> bool {
        if self.scroll_offset == usize::MAX {
            return true;
        }
        let Some(height) = self.last_content_height else {
            return false;
        };
        if self.renderables.is_empty() {
            return true;
        }
        let Some(total_height) = self.last_rendered_height else {
            return false;
        };
        if total_height <= height {
            return true;
        }
        let max_scroll = total_height.saturating_sub(height);
        self.scroll_offset >= max_scroll
    }

    /// Request that the given text chunk index be scrolled into view on next render.
    pub(super) fn scroll_chunk_into_view(&mut self, chunk_index: usize) {
        self.pending_scroll_chunk = Some(chunk_index);
    }

    pub(super) fn ensure_chunk_visible(&mut self, idx: usize, area: Rect) {
        if area.height == 0 || idx >= self.renderables.len() {
            return;
        }
        let first = self
            .renderables
            .iter()
            .take(idx)
            .map(|r| r.desired_height(area.width) as usize)
            .sum();
        let last = first + self.renderables[idx].desired_height(area.width) as usize;
        let current_top = self.scroll_offset;
        let current_bottom = current_top.saturating_add(area.height.saturating_sub(1) as usize);
        if first < current_top {
            self.scroll_offset = first;
        } else if last > current_bottom {
            self.scroll_offset = last.saturating_sub(area.height.saturating_sub(1) as usize);
        }
    }
}
