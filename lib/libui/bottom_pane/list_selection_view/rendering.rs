use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use crate::render::renderable::Renderable;

use super::super::popup_consts::MAX_POPUP_ROWS;
use super::super::selection_popup_common::measure_rows_height;
use super::super::selection_popup_common::measure_rows_height_stable_col_widths;
use super::super::selection_popup_common::measure_rows_height_with_col_width_mode;
use super::super::selection_popup_common::render_menu_surface;
use super::super::selection_popup_common::render_rows;
use super::super::selection_popup_common::render_rows_stable_col_widths;
use super::super::selection_popup_common::render_rows_with_col_width_mode;
use super::super::selection_popup_common::wrap_styled_line;
use super::selection_logic::ListSelectionView;
use super::types::ColumnWidthMode;
use super::types::SIDE_CONTENT_GAP;
use super::types::popup_content_width;

impl Renderable for ListSelectionView {
    fn desired_height(&self, width: u16) -> u16 {
        // Inner content width after menu surface horizontal insets (2 per side).
        let inner_width = popup_content_width(width);

        // When side-by-side is active, measure the list at the reduced width
        // that accounts for the gap and side panel.
        let effective_rows_width = if let Some(side_w) = self.side_layout_width(inner_width) {
            Self::rows_width(width).saturating_sub(SIDE_CONTENT_GAP + side_w)
        } else {
            Self::rows_width(width)
        };

        // Measure wrapped height for up to MAX_POPUP_ROWS items.
        let rows = self.build_rows();
        let rows_height = match self.col_width_mode {
            ColumnWidthMode::AutoVisible => measure_rows_height(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
            ),
            ColumnWidthMode::AutoAllRows => measure_rows_height_stable_col_widths(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
            ),
            ColumnWidthMode::Fixed => measure_rows_height_with_col_width_mode(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
                ColumnWidthMode::Fixed,
            ),
        };

        let mut height = self.header.desired_height(inner_width);
        height = height.saturating_add(rows_height + 3);
        if self.is_searchable {
            height = height.saturating_add(1);
        }

        // Side content: when the terminal is wide enough the panel sits beside
        // the list and shares vertical space; otherwise it stacks below.
        if self.side_layout_width(inner_width).is_some() {
            // Side-by-side — side content shares list rows vertically so it
            // doesn't add to total height.
        } else {
            let side_h = self.stacked_side_content().desired_height(inner_width);
            if side_h > 0 {
                height = height.saturating_add(1 + side_h);
            }
        }

        if let Some(note) = &self.footer_note {
            let note_width = width.saturating_sub(2);
            let note_lines = wrap_styled_line(note, note_width);
            height = height.saturating_add(note_lines.len() as u16);
        }
        if self.footer_hint.is_some() {
            height = height.saturating_add(1);
        }
        height
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let note_width = area.width.saturating_sub(2);
        let note_lines = self
            .footer_note
            .as_ref()
            .map(|note| wrap_styled_line(note, note_width));
        let note_height = note_lines.as_ref().map_or(0, |lines| lines.len() as u16);
        let footer_rows = note_height + u16::from(self.footer_hint.is_some());
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(footer_rows)]).areas(area);

        let outer_content_area = content_area;
        // Paint the shared menu surface and then layout inside the returned inset.
        let content_area = render_menu_surface(outer_content_area, buf);

        let inner_width = popup_content_width(outer_content_area.width);
        let side_w = self.side_layout_width(inner_width);

        // When side-by-side is active, shrink the list to make room.
        let full_rows_width = Self::rows_width(outer_content_area.width);
        let effective_rows_width = if let Some(sw) = side_w {
            full_rows_width.saturating_sub(SIDE_CONTENT_GAP + sw)
        } else {
            full_rows_width
        };

        let header_height = self.header.desired_height(inner_width);
        let rows = self.build_rows();
        let rows_height = match self.col_width_mode {
            ColumnWidthMode::AutoVisible => measure_rows_height(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
            ),
            ColumnWidthMode::AutoAllRows => measure_rows_height_stable_col_widths(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
            ),
            ColumnWidthMode::Fixed => measure_rows_height_with_col_width_mode(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
                ColumnWidthMode::Fixed,
            ),
        };

        // Stacked (fallback) side content height — only used when not side-by-side.
        let stacked_side_h = if side_w.is_none() {
            self.stacked_side_content().desired_height(inner_width)
        } else {
            0
        };
        let stacked_gap = if stacked_side_h > 0 { 1 } else { 0 };

        let [header_area, _, search_area, list_area, _, stacked_side_area] = Layout::vertical([
            Constraint::Max(header_height),
            Constraint::Max(1),
            Constraint::Length(if self.is_searchable { 1 } else { 0 }),
            Constraint::Length(rows_height),
            Constraint::Length(stacked_gap),
            Constraint::Length(stacked_side_h),
        ])
        .areas(content_area);

        // -- Header --
        if header_area.height < header_height {
            let [header_area, elision_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(header_area);
            self.header.render(header_area, buf);
            Paragraph::new(vec![
                Line::from(format!("[… {header_height} lines] ctrl + a view all")).dim(),
            ])
            .render(elision_area, buf);
        } else {
            self.header.render(header_area, buf);
        }

        // -- Search bar --
        if self.is_searchable {
            Line::from(self.search_query.clone()).render(search_area, buf);
            let query_span: Span<'static> = if self.search_query.is_empty() {
                self.search_placeholder
                    .as_ref()
                    .map(|placeholder| placeholder.clone().dim())
                    .unwrap_or_else(|| "".into())
            } else {
                self.search_query.clone().into()
            };
            Line::from(query_span).render(search_area, buf);
        }

        // -- List rows --
        if list_area.height > 0 {
            let render_area = Rect {
                x: list_area.x.saturating_sub(2),
                y: list_area.y,
                width: effective_rows_width.max(1),
                height: list_area.height,
            };
            match self.col_width_mode {
                ColumnWidthMode::AutoVisible => render_rows(
                    render_area,
                    buf,
                    &rows,
                    &self.state,
                    render_area.height as usize,
                    "no matches",
                ),
                ColumnWidthMode::AutoAllRows => render_rows_stable_col_widths(
                    render_area,
                    buf,
                    &rows,
                    &self.state,
                    render_area.height as usize,
                    "no matches",
                ),
                ColumnWidthMode::Fixed => render_rows_with_col_width_mode(
                    render_area,
                    buf,
                    &rows,
                    &self.state,
                    render_area.height as usize,
                    "no matches",
                    ColumnWidthMode::Fixed,
                ),
            };
        }

        // -- Side content (preview panel) --
        if let Some(sw) = side_w {
            // Side-by-side: render to the right half of the popup content
            // area so preview content can center vertically in that panel.
            let side_x = content_area.x + content_area.width - sw;
            let side_area = Rect::new(side_x, content_area.y, sw, content_area.height);

            // Clear the menu-surface background behind the side panel so the
            // preview appears on the terminal's own background.
            let clear_x = side_x.saturating_sub(SIDE_CONTENT_GAP);
            let clear_w = outer_content_area
                .x
                .saturating_add(outer_content_area.width)
                .saturating_sub(clear_x);
            Self::clear_to_terminal_bg(
                buf,
                Rect::new(
                    clear_x,
                    outer_content_area.y,
                    clear_w,
                    outer_content_area.height,
                ),
            );
            self.side_content.render(side_area, buf);
            if !self.preserve_side_content_bg {
                Self::force_bg_to_terminal_bg(
                    buf,
                    Rect::new(
                        clear_x,
                        outer_content_area.y,
                        clear_w,
                        outer_content_area.height,
                    ),
                );
            }
        } else if stacked_side_area.height > 0 {
            // Stacked fallback: render below the list (same as old footer_content).
            let clear_height = (outer_content_area.y + outer_content_area.height)
                .saturating_sub(stacked_side_area.y);
            let clear_area = Rect::new(
                outer_content_area.x,
                stacked_side_area.y,
                outer_content_area.width,
                clear_height,
            );
            Self::clear_to_terminal_bg(buf, clear_area);
            self.stacked_side_content().render(stacked_side_area, buf);
        }

        if footer_area.height > 0 {
            let [note_area, hint_area] = Layout::vertical([
                Constraint::Length(note_height),
                Constraint::Length(if self.footer_hint.is_some() { 1 } else { 0 }),
            ])
            .areas(footer_area);

            if let Some(lines) = note_lines {
                let note_area = Rect {
                    x: note_area.x + 2,
                    y: note_area.y,
                    width: note_area.width.saturating_sub(2),
                    height: note_area.height,
                };
                for (idx, line) in lines.iter().enumerate() {
                    if idx as u16 >= note_area.height {
                        break;
                    }
                    let line_area = Rect {
                        x: note_area.x,
                        y: note_area.y + idx as u16,
                        width: note_area.width,
                        height: 1,
                    };
                    line.clone().render(line_area, buf);
                }
            }

            if let Some(hint) = &self.footer_hint {
                let hint_area = Rect {
                    x: hint_area.x + 2,
                    y: hint_area.y,
                    width: hint_area.width.saturating_sub(2),
                    height: hint_area.height,
                };
                hint.clone().dim().render(hint_area, buf);
            }
        }
    }
}
