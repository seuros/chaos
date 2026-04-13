use ratatui::text::Line;
use ratatui::text::Span;

use crate::app_event_sender::AppEventSender;
use crate::key_hint::KeyBinding;
use crate::render::renderable::Renderable;

pub use super::super::selection_popup_common::ColumnWidthMode;

/// Minimum list width (in content columns) required before the side-by-side
/// layout is activated. Keeps the list usable even when sharing horizontal
/// space with the side content panel.
pub(super) const MIN_LIST_WIDTH_FOR_SIDE: u16 = 40;

/// Horizontal gap (in columns) between the list area and the side content
/// panel when side-by-side layout is active.
pub(super) const SIDE_CONTENT_GAP: u16 = 2;

/// Shared menu-surface horizontal inset (2 cells per side) used by selection popups.
pub(super) const MENU_SURFACE_HORIZONTAL_INSET: u16 = 4;

/// Controls how the side content panel is sized relative to the popup width.
///
/// When the computed side width falls below `side_content_min_width` or the
/// remaining list area would be narrower than [`MIN_LIST_WIDTH_FOR_SIDE`], the
/// side-by-side layout is abandoned and the stacked fallback is used instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideContentWidth {
    /// Fixed number of columns.  `Fixed(0)` disables side content entirely.
    Fixed(u16),
    /// Exact 50/50 split of the content area (minus the inter-column gap).
    Half,
}

impl Default for SideContentWidth {
    fn default() -> Self {
        Self::Fixed(0)
    }
}

/// Returns the popup content width after subtracting the shared menu-surface
/// horizontal inset (2 columns on each side).
pub fn popup_content_width(total_width: u16) -> u16 {
    total_width.saturating_sub(MENU_SURFACE_HORIZONTAL_INSET)
}

/// Returns side-by-side layout widths as `(list_width, side_width)` when the
/// layout can fit. Returns `None` when the side panel is disabled/too narrow or
/// when the remaining list width would become unusably small.
pub fn side_by_side_layout_widths(
    content_width: u16,
    side_content_width: SideContentWidth,
    side_content_min_width: u16,
) -> Option<(u16, u16)> {
    let side_width = match side_content_width {
        SideContentWidth::Fixed(0) => return None,
        SideContentWidth::Fixed(width) => width,
        SideContentWidth::Half => content_width.saturating_sub(SIDE_CONTENT_GAP) / 2,
    };
    if side_width < side_content_min_width {
        return None;
    }
    let list_width = content_width.saturating_sub(SIDE_CONTENT_GAP + side_width);
    (list_width >= MIN_LIST_WIDTH_FOR_SIDE).then_some((list_width, side_width))
}

/// One selectable item in the generic selection list.
pub type SelectionAction = Box<dyn Fn(&AppEventSender) + Send + Sync>;

/// Callback invoked whenever the highlighted item changes (arrow keys, search
/// filter, number-key jump).  Receives the *actual* index into the unfiltered
/// `items` list and the event sender.  Used by the theme picker for live preview.
pub type OnSelectionChangedCallback = Option<Box<dyn Fn(usize, &AppEventSender) + Send + Sync>>;

/// Callback invoked when the picker is dismissed without accepting (Esc or
/// Ctrl+C).  Used by the theme picker to restore the pre-open theme.
pub type OnCancelCallback = Option<Box<dyn Fn(&AppEventSender) + Send + Sync>>;

/// One row in a [`ListSelectionView`] selection list.
///
/// This is the source-of-truth model for row state before filtering and
/// formatting into render rows. A row is treated as disabled when either
/// `is_disabled` is true or `disabled_reason` is present; disabled rows cannot
/// be accepted and are skipped by keyboard navigation.
#[derive(Default)]
pub struct SelectionItem {
    pub name: String,
    pub name_prefix_spans: Vec<Span<'static>>,
    pub display_shortcut: Option<KeyBinding>,
    pub description: Option<String>,
    pub selected_description: Option<String>,
    pub is_current: bool,
    pub is_default: bool,
    pub is_disabled: bool,
    pub actions: Vec<SelectionAction>,
    pub dismiss_on_select: bool,
    pub search_value: Option<String>,
    pub disabled_reason: Option<String>,
}

/// Construction-time configuration for [`ListSelectionView`].
///
/// This config is consumed once by [`ListSelectionView::new`]. After
/// construction, mutable interaction state (filtering, scrolling, and selected
/// row) lives on the view itself.
///
/// `col_width_mode` controls column width mode in selection lists:
/// `AutoVisible` (default) measures only rows visible in the viewport
/// `AutoAllRows` measures all rows to ensure stable column widths as the user scrolls
/// `Fixed` used a fixed 30/70  split between columns
pub struct SelectionViewParams {
    pub view_id: Option<&'static str>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub footer_note: Option<Line<'static>>,
    pub footer_hint: Option<Line<'static>>,
    pub items: Vec<SelectionItem>,
    pub is_searchable: bool,
    pub search_placeholder: Option<String>,
    pub col_width_mode: ColumnWidthMode,
    pub header: Box<dyn Renderable>,
    pub initial_selected_idx: Option<usize>,

    /// Rich content rendered beside (wide terminals) or below (narrow terminals)
    /// the list items, inside the bordered menu surface. Used by the theme picker
    /// to show a syntax-highlighted preview.
    pub side_content: Box<dyn Renderable>,

    /// Width mode for side content when side-by-side layout is active.
    pub side_content_width: SideContentWidth,

    /// Minimum side panel width required before side-by-side layout activates.
    pub side_content_min_width: u16,

    /// Optional fallback content rendered when side-by-side does not fit.
    /// When absent, `side_content` is reused.
    pub stacked_side_content: Option<Box<dyn Renderable>>,

    /// Keep side-content background colors after rendering in side-by-side mode.
    /// Disabled by default so existing popups preserve their reset-background look.
    pub preserve_side_content_bg: bool,

    /// Called when the highlighted item changes (navigation, filter, number-key).
    /// Receives the *actual* item index, not the filtered/visible index.
    pub on_selection_changed: OnSelectionChangedCallback,

    /// Called when the picker is dismissed via Esc/Ctrl+C without selecting.
    pub on_cancel: OnCancelCallback,
}

impl Default for SelectionViewParams {
    fn default() -> Self {
        Self {
            view_id: None,
            title: None,
            subtitle: None,
            footer_note: None,
            footer_hint: None,
            items: Vec::new(),
            is_searchable: false,
            search_placeholder: None,
            col_width_mode: ColumnWidthMode::AutoVisible,
            header: Box::new(()),
            initial_selected_idx: None,
            side_content: Box::new(()),
            side_content_width: SideContentWidth::default(),
            side_content_min_width: 0,
            stacked_side_content: None,
            preserve_side_content_bg: false,
            on_selection_changed: None,
            on_cancel: None,
        }
    }
}
