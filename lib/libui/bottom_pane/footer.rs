//! The bottom-pane footer renders transient hints and context indicators.
//!
//! The footer is pure rendering: it formats `FooterProps` into `Line`s without mutating any state.
//! It intentionally does not decide *which* footer content should be shown; that is owned by the
//! `ChatComposer` (which selects a `FooterMode`) and by higher-level state machines like
//! `ChatWidget` (which decides when quit/interrupt is allowed).
//!
//! Some footer content is time-based rather than event-based, such as the "press again to quit"
//! hint. The owning widgets schedule redraws so time-based hints can expire even if the UI is
//! otherwise idle.
//!
//! Terminology used in this module:
//! - "status line" means the contextual row rendered by the Lua statusline hook.
//! - "instructional footer" means a row that tells the user what to do next, such as quit
//!   confirmation, shortcut help, or queue hints.
//! - "contextual footer" means the footer is free to show ambient context instead of an
//!   instruction. In that state, the footer may render the configured status line, the active
//!   agent label, or both combined.
//!
//! Single-line collapse overview:
//! 1. The composer decides the current `FooterMode` and hint flags, then calls
//!    `single_line_footer_layout` for the base single-line modes.
//! 2. `single_line_footer_layout` applies the width-based fallback rules:
//!    (If this description is hard to follow, just try it out by resizing
//!    your terminal width; these rules were built out of trial and error.)
//!    - Start with the fullest left-side hint plus the right-side context.
//!    - When the queue hint is active, prefer keeping that queue hint visible,
//!      even if it means dropping the right-side context earlier; the queue
//!      hint may also be shortened before it is removed.
//!    - When the queue hint is not active but the mode cycle hint is applicable,
//!      drop "? for shortcuts" before dropping "(shift+tab to cycle)".
//!    - If "(shift+tab to cycle)" cannot fit, also hide the right-side
//!      context to avoid too many state transitions in quick succession.
//!    - Finally, try a mode-only line (with and without context), and fall
//!      back to no left-side footer if nothing can fit.
//! 3. When collapse chooses a specific line, callers render it via
//!    `render_footer_line`. Otherwise, callers render the straightforward
//!    mode-to-text mapping via `render_footer_from_props`.
//!
//! In short: `single_line_footer_layout` chooses *what* best fits, and the two
//! render helpers choose whether to draw the chosen line or the default
//! `FooterProps` mapping.

mod collaboration_mode;
mod render;
mod shortcuts;
mod types;

pub use render::best_right_mode_indicator_line;
pub use render::can_show_left_with_context;
pub use render::context_window_line;
pub use render::esc_hint_mode;
pub use render::footer_height;
pub use render::footer_hint_items_width;
pub use render::footer_line_width;
pub use render::inset_footer_hint_area;
pub use render::max_left_width_for_right;
pub use render::passive_footer_status_line;
pub use render::render_context_right;
pub use render::render_footer_from_props;
pub use render::render_footer_hint_items;
pub use render::render_footer_line;
pub use render::reset_mode_after_activity;
pub use render::single_line_footer_layout;
pub use render::toggle_shortcut_mode;
pub use render::uses_passive_footer_status_layout;
pub use types::CollaborationModeIndicator;
pub use types::FooterMode;
pub use types::FooterProps;
pub use types::SummaryLeft;

#[cfg(test)]
pub(crate) mod tests;
