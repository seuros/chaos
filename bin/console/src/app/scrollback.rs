use super::{App, PaneId, Renderable, tui};
use crate::tile_manager::Scroll;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Widget;

impl App {
    /// Resolve wheel/page movement against the chat pane, not the composer-sized
    /// inline viewport or a full-screen transcript overlay.
    pub(super) fn scroll_chat(&mut self, tui: &mut tui::Tui, scroll: Scroll) {
        if matches!(scroll, Scroll::End) {
            self.tile_manager.chat_scrollback.follow_live();
        } else {
            if matches!(scroll, Scroll::Down(_)) && !self.tile_manager.chat_scrollback.is_scrolled()
            {
                return;
            }
            let area = self.chat_scroll_area(tui);
            let height = area.height.saturating_sub(
                self.chat_widget
                    .bottom_pane_renderable()
                    .desired_height(area.width),
            );
            self.tile_manager.chat_scrollback.sync(
                &self.transcript_cells,
                area.width,
                self.chat_widget.active_cell_transcript_key(),
                |width| self.chat_widget.active_cell_display_lines(width),
            );
            self.tile_manager.chat_scrollback.scroll(scroll, height);
        }
        if self.tile_manager.chat_scrollback.is_scrolled() {
            self.tile_manager.require_inline_history_restore();
        }
        self.reset_backtrack_state();
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn chat_scroll_area(&self, tui: &tui::Tui) -> Rect {
        if !self.tile_manager.is_single_pane()
            && let Some(area) = self.tile_manager.pane_rect(PaneId::ROOT)
        {
            return area;
        }
        let size = tui.terminal.last_known_screen_size;
        let top = tui
            .top_reserved_rows()
            .saturating_add(u16::from(self.agent_navigation.has_tabs()))
            .min(size.height);
        Rect::new(0, top, size.width, size.height.saturating_sub(top))
    }

    pub(super) fn chat_scroll_page_height(&self, tui: &tui::Tui) -> usize {
        let area = self.chat_scroll_area(tui);
        usize::from(
            area.height
                .saturating_sub(
                    self.chat_widget
                        .bottom_pane_renderable()
                        .desired_height(area.width),
                )
                .saturating_sub(1)
                .max(1),
        )
    }

    /// Keep the actual composer (including status and dialogs) pinned while
    /// history and the in-flight cell share one scrollable body.
    pub(super) fn render_scrolled_chat(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
    ) -> Option<(u16, u16)> {
        let bottom = self.chat_widget.bottom_pane_renderable();
        let bottom_height = bottom.desired_height(area.width).min(area.height);
        let body = Rect {
            height: area.height.saturating_sub(bottom_height),
            ..area
        };
        let composer = Rect {
            y: body.bottom(),
            height: bottom_height,
            ..area
        };
        self.tile_manager.chat_scrollback.sync(
            &self.transcript_cells,
            area.width,
            self.chat_widget.active_cell_transcript_key(),
            |width| self.chat_widget.active_cell_display_lines(width),
        );
        self.tile_manager.chat_scrollback.render(body, buf);
        self.chat_widget.render_bottom_pane(composer, buf);
        // Reuse the composer's existing top padding; no extra chrome or pager.
        let hint = if self.chat_widget.no_modal_or_popup_active() {
            "Scrollback · End / Esc to follow live"
        } else if self.chat_widget.allows_transcript_scroll() {
            "Scrollback · End to follow live"
        } else {
            "Scrollback"
        };
        Line::from(hint).dim().render(
            Rect {
                height: bottom_height.min(1),
                ..composer
            },
            buf,
        );
        bottom.cursor_pos(composer)
    }
}
