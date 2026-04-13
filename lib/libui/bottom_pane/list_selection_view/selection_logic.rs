use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use itertools::Itertools as _;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use crate::app_event_sender::AppEventSender;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;

use super::super::CancellationEvent;
use super::super::bottom_pane_view::BottomPaneView;
use super::super::popup_consts::MAX_POPUP_ROWS;
use super::super::scroll_state::ScrollState;
use super::super::selection_popup_common::GenericDisplayRow;
use super::types::ColumnWidthMode;
use super::types::OnCancelCallback;
use super::types::OnSelectionChangedCallback;
use super::types::SelectionItem;
use super::types::SelectionViewParams;
use super::types::SideContentWidth;
use super::types::side_by_side_layout_widths;

/// Runtime state for rendering and interacting with a list-based selection popup.
///
/// This type is the single authority for filtered index mapping between
/// visible rows and source items and for preserving selection while filters
/// change.
pub struct ListSelectionView {
    pub(super) view_id: Option<&'static str>,
    pub(super) footer_note: Option<Line<'static>>,
    pub(super) footer_hint: Option<Line<'static>>,
    pub(super) items: Vec<SelectionItem>,
    pub(super) state: ScrollState,
    pub(super) complete: bool,
    pub(super) app_event_tx: AppEventSender,
    pub(super) is_searchable: bool,
    pub(super) search_query: String,
    pub(super) search_placeholder: Option<String>,
    pub(super) col_width_mode: ColumnWidthMode,
    pub(super) filtered_indices: Vec<usize>,
    pub(super) last_selected_actual_idx: Option<usize>,
    pub(super) header: Box<dyn Renderable>,
    pub(super) initial_selected_idx: Option<usize>,
    pub(super) side_content: Box<dyn Renderable>,
    pub(super) side_content_width: SideContentWidth,
    pub(super) side_content_min_width: u16,
    pub(super) stacked_side_content: Option<Box<dyn Renderable>>,
    pub(super) preserve_side_content_bg: bool,

    /// Called when the highlighted item changes (navigation, filter, number-key).
    pub(super) on_selection_changed: OnSelectionChangedCallback,

    /// Called when the picker is dismissed via Esc/Ctrl+C without selecting.
    pub(super) on_cancel: OnCancelCallback,
}

impl ListSelectionView {
    /// Create a selection popup view with filtering, scrolling, and callbacks wired.
    ///
    /// The constructor normalizes header/title composition and immediately
    /// applies filtering so `ScrollState` starts in a valid visible range.
    /// When search is enabled, rows without `search_value` will disappear as
    /// soon as the query is non-empty, which can look like dropped data unless
    /// callers intentionally populate that field.
    pub fn new(params: SelectionViewParams, app_event_tx: AppEventSender) -> Self {
        let mut header = params.header;
        if params.title.is_some() || params.subtitle.is_some() {
            let title = params.title.map(|title| Line::from(title.bold()));
            let subtitle = params.subtitle.map(|subtitle| Line::from(subtitle.dim()));
            header = Box::new(ColumnRenderable::with([
                header,
                Box::new(title),
                Box::new(subtitle),
            ]));
        }
        let mut s = Self {
            view_id: params.view_id,
            footer_note: params.footer_note,
            footer_hint: params.footer_hint,
            items: params.items,
            state: ScrollState::new(),
            complete: false,
            app_event_tx,
            is_searchable: params.is_searchable,
            search_query: String::new(),
            search_placeholder: if params.is_searchable {
                params.search_placeholder
            } else {
                None
            },
            col_width_mode: params.col_width_mode,
            filtered_indices: Vec::new(),
            last_selected_actual_idx: None,
            header,
            initial_selected_idx: params.initial_selected_idx,
            side_content: params.side_content,
            side_content_width: params.side_content_width,
            side_content_min_width: params.side_content_min_width,
            stacked_side_content: params.stacked_side_content,
            preserve_side_content_bg: params.preserve_side_content_bg,
            on_selection_changed: params.on_selection_changed,
            on_cancel: params.on_cancel,
        };
        s.apply_filter();
        s
    }

    pub(super) fn visible_len(&self) -> usize {
        self.filtered_indices.len()
    }

    pub(super) fn max_visible_rows(len: usize) -> usize {
        MAX_POPUP_ROWS.min(len.max(1))
    }

    pub(super) fn selected_actual_idx(&self) -> Option<usize> {
        self.state
            .selected_idx
            .and_then(|visible_idx| self.filtered_indices.get(visible_idx).copied())
    }

    pub(super) fn apply_filter(&mut self) {
        let previously_selected = self
            .selected_actual_idx()
            .or_else(|| {
                (!self.is_searchable)
                    .then(|| self.items.iter().position(|item| item.is_current))
                    .flatten()
            })
            .or_else(|| self.initial_selected_idx.take());

        if self.is_searchable && !self.search_query.is_empty() {
            let query_lower = self.search_query.to_lowercase();
            self.filtered_indices = self
                .items
                .iter()
                .positions(|item| {
                    item.search_value
                        .as_ref()
                        .is_some_and(|v| v.to_lowercase().contains(&query_lower))
                })
                .collect();
        } else {
            self.filtered_indices = (0..self.items.len()).collect();
        }

        let len = self.filtered_indices.len();
        self.state.selected_idx = self
            .state
            .selected_idx
            .and_then(|visible_idx| {
                self.filtered_indices
                    .get(visible_idx)
                    .and_then(|idx| self.filtered_indices.iter().position(|cur| cur == idx))
            })
            .or_else(|| {
                previously_selected.and_then(|actual_idx| {
                    self.filtered_indices
                        .iter()
                        .position(|idx| *idx == actual_idx)
                })
            })
            .or_else(|| (len > 0).then_some(0));

        let visible = Self::max_visible_rows(len);
        self.state.clamp_selection(len);
        self.state.ensure_visible(len, visible);

        // Notify the callback when filtering changes the selected actual item
        // so live preview stays in sync (e.g. typing in the theme picker).
        let new_actual = self.selected_actual_idx();
        if new_actual != previously_selected {
            self.fire_selection_changed();
        }
    }

    pub(super) fn build_rows(&self) -> Vec<GenericDisplayRow> {
        self.filtered_indices
            .iter()
            .enumerate()
            .filter_map(|(visible_idx, actual_idx)| {
                self.items.get(*actual_idx).map(|item| {
                    let is_selected = self.state.selected_idx == Some(visible_idx);
                    let prefix = if is_selected { '›' } else { ' ' };
                    let name = item.name.as_str();
                    let marker = if item.is_current {
                        " (current)"
                    } else if item.is_default {
                        " (default)"
                    } else {
                        ""
                    };
                    let name_with_marker = format!("{name}{marker}");
                    let n = visible_idx + 1;
                    let wrap_prefix = if self.is_searchable {
                        // The number keys don't work when search is enabled (since we let the
                        // numbers be used for the search query).
                        format!("{prefix} ")
                    } else {
                        format!("{prefix} {n}. ")
                    };
                    let wrap_prefix_width = UnicodeWidthStr::width(wrap_prefix.as_str());
                    let mut name_prefix_spans = Vec::new();
                    name_prefix_spans.push(wrap_prefix.into());
                    name_prefix_spans.extend(item.name_prefix_spans.clone());
                    let description = is_selected
                        .then(|| item.selected_description.clone())
                        .flatten()
                        .or_else(|| item.description.clone());
                    let wrap_indent = description.is_none().then_some(wrap_prefix_width);
                    let is_disabled = item.is_disabled || item.disabled_reason.is_some();
                    GenericDisplayRow {
                        name: name_with_marker,
                        name_prefix_spans,
                        display_shortcut: item.display_shortcut,
                        match_indices: None,
                        description,
                        category_tag: None,
                        wrap_indent,
                        is_disabled,
                        disabled_reason: item.disabled_reason.clone(),
                    }
                })
            })
            .collect()
    }

    pub(super) fn move_up(&mut self) {
        let before = self.selected_actual_idx();
        let len = self.visible_len();
        self.state.move_up_wrap(len);
        let visible = Self::max_visible_rows(len);
        self.state.ensure_visible(len, visible);
        self.skip_disabled_up();
        if self.selected_actual_idx() != before {
            self.fire_selection_changed();
        }
    }

    pub(super) fn move_down(&mut self) {
        let before = self.selected_actual_idx();
        let len = self.visible_len();
        self.state.move_down_wrap(len);
        let visible = Self::max_visible_rows(len);
        self.state.ensure_visible(len, visible);
        self.skip_disabled_down();
        if self.selected_actual_idx() != before {
            self.fire_selection_changed();
        }
    }

    pub(super) fn fire_selection_changed(&self) {
        if let Some(cb) = &self.on_selection_changed
            && let Some(actual) = self.selected_actual_idx()
        {
            cb(actual, &self.app_event_tx);
        }
    }

    pub(super) fn accept(&mut self) {
        let selected_item = self
            .state
            .selected_idx
            .and_then(|idx| self.filtered_indices.get(idx))
            .and_then(|actual_idx| self.items.get(*actual_idx));
        if let Some(item) = selected_item
            && item.disabled_reason.is_none()
            && !item.is_disabled
        {
            if let Some(idx) = self.state.selected_idx
                && let Some(actual_idx) = self.filtered_indices.get(idx)
            {
                self.last_selected_actual_idx = Some(*actual_idx);
            }
            for act in &item.actions {
                act(&self.app_event_tx);
            }
            if item.dismiss_on_select {
                self.complete = true;
            }
        } else if selected_item.is_none() {
            if let Some(cb) = &self.on_cancel {
                cb(&self.app_event_tx);
            }
            self.complete = true;
        }
    }

    #[cfg(test)]
    pub fn set_search_query(&mut self, query: String) {
        self.search_query = query;
        self.apply_filter();
    }

    pub fn take_last_selected_index(&mut self) -> Option<usize> {
        self.last_selected_actual_idx.take()
    }

    pub(super) fn rows_width(total_width: u16) -> u16 {
        total_width.saturating_sub(2)
    }

    pub(super) fn clear_to_terminal_bg(buf: &mut Buffer, area: Rect) {
        let buf_area = buf.area();
        let min_x = area.x.max(buf_area.x);
        let min_y = area.y.max(buf_area.y);
        let max_x = area
            .x
            .saturating_add(area.width)
            .min(buf_area.x.saturating_add(buf_area.width));
        let max_y = area
            .y
            .saturating_add(area.height)
            .min(buf_area.y.saturating_add(buf_area.height));
        for y in min_y..max_y {
            for x in min_x..max_x {
                buf[(x, y)]
                    .set_symbol(" ")
                    .set_style(ratatui::style::Style::reset());
            }
        }
    }

    pub(super) fn force_bg_to_terminal_bg(buf: &mut Buffer, area: Rect) {
        let buf_area = buf.area();
        let min_x = area.x.max(buf_area.x);
        let min_y = area.y.max(buf_area.y);
        let max_x = area
            .x
            .saturating_add(area.width)
            .min(buf_area.x.saturating_add(buf_area.width));
        let max_y = area
            .y
            .saturating_add(area.height)
            .min(buf_area.y.saturating_add(buf_area.height));
        for y in min_y..max_y {
            for x in min_x..max_x {
                buf[(x, y)].set_bg(ratatui::style::Color::Reset);
            }
        }
    }

    pub(super) fn stacked_side_content(&self) -> &dyn Renderable {
        self.stacked_side_content
            .as_deref()
            .unwrap_or_else(|| self.side_content.as_ref())
    }

    /// Returns `Some(side_width)` when the content area is wide enough for a
    /// side-by-side layout (list + gap + side panel), `None` otherwise.
    pub(super) fn side_layout_width(&self, content_width: u16) -> Option<u16> {
        side_by_side_layout_widths(
            content_width,
            self.side_content_width,
            self.side_content_min_width,
        )
        .map(|(_, side_width)| side_width)
    }

    pub(super) fn skip_disabled_down(&mut self) {
        let len = self.visible_len();
        for _ in 0..len {
            if let Some(idx) = self.state.selected_idx
                && let Some(actual_idx) = self.filtered_indices.get(idx)
                && self
                    .items
                    .get(*actual_idx)
                    .is_some_and(|item| item.disabled_reason.is_some() || item.is_disabled)
            {
                self.state.move_down_wrap(len);
            } else {
                break;
            }
        }
    }

    pub(super) fn skip_disabled_up(&mut self) {
        let len = self.visible_len();
        for _ in 0..len {
            if let Some(idx) = self.state.selected_idx
                && let Some(actual_idx) = self.filtered_indices.get(idx)
                && self
                    .items
                    .get(*actual_idx)
                    .is_some_and(|item| item.disabled_reason.is_some() || item.is_disabled)
            {
                self.state.move_up_wrap(len);
            } else {
                break;
            }
        }
    }
}

impl BottomPaneView for ListSelectionView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            // Some terminals (or configurations) send Control key chords as
            // C0 control characters without reporting the CONTROL modifier.
            // Handle fallbacks for Ctrl-P/N here so navigation works everywhere.
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{0010}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^P */ => self.move_up(),
            KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::NONE,
                ..
            } if !self.is_searchable => self.move_up(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{000e}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^N */ => self.move_down(),
            KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::NONE,
                ..
            } if !self.is_searchable => self.move_down(),
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } if self.is_searchable => {
                self.search_query.pop();
                self.apply_filter();
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.on_ctrl_c();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if self.is_searchable
                && !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.search_query.push(c);
                self.apply_filter();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !self.is_searchable
                && !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(idx) = c
                    .to_digit(10)
                    .map(|d| d as usize)
                    .and_then(|d| d.checked_sub(1))
                    && idx < self.items.len()
                    && self
                        .items
                        .get(idx)
                        .is_some_and(|item| item.disabled_reason.is_none() && !item.is_disabled)
                {
                    self.state.selected_idx = Some(idx);
                    self.accept();
                }
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.accept(),
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn view_id(&self) -> Option<&'static str> {
        self.view_id
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_actual_idx()
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if let Some(cb) = &self.on_cancel {
            cb(&self.app_event_tx);
        }
        self.complete = true;
        CancellationEvent::Handled
    }
}
