//! The bottom pane is the interactive footer of the chat UI.
//!
//! The pane owns the [`ChatComposer`] (editable prompt input) and a stack of transient
//! [`BottomPaneView`]s (popups/modals) that temporarily replace the composer for focused
//! interactions like selection lists.
//!
//! Input routing is layered: `BottomPane` decides which local surface receives a key (view vs
//! composer), while higher-level intent such as "interrupt" or "quit" is decided by the parent
//! widget (`ChatWidget`). This split matters for Ctrl+C/Ctrl+D: the bottom pane gives the active
//! view the first chance to consume Ctrl+C (typically to dismiss itself), and `ChatWidget` may
//! treat an unhandled Ctrl+C as an interrupt or as the first press of a double-press quit
//! shortcut.
//!
//! Some UI is time-based rather than input-based, such as the transient "press again to quit"
//! hint. The pane schedules redraws so those hints can expire even when the UI is otherwise idle.
use std::path::PathBuf;

use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::pending_input_preview::PendingInputPreview;
use crate::bottom_pane::pending_process_approvals::PendingProcessApprovals;
use crate::bottom_pane::unified_exec_footer::UnifiedExecFooter;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableItem;
use crate::tui::FrameRequester;
pub use bottom_pane_view::BottomPaneView;
use chaos_ipc::request_user_input::RequestUserInputEvent;
use chaos_ipc::user_input::TextElement;
use chaos_locate::FileMatch;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::time::Duration;

mod app_link_view;
mod approval_overlay;
mod form_layout;
mod mcp_add_form;
mod mcp_server_elicitation;
mod reflex_setup;
mod request_user_input;
mod single_line_input;
pub use app_link_view::AppLinkElicitationTarget;
pub use app_link_view::AppLinkSuggestionType;
pub use app_link_view::AppLinkView;
pub use app_link_view::AppLinkViewParams;
pub use approval_overlay::ApprovalOverlay;
pub use approval_overlay::ApprovalRequest;
pub use approval_overlay::format_requested_permissions_rule;
pub use mcp_add_form::McpAddForm;
pub use mcp_server_elicitation::McpServerElicitationFormRequest;
pub use mcp_server_elicitation::McpServerElicitationOverlay;
pub use reflex_setup::ReflexSetupForm;
pub use request_user_input::RequestUserInputOverlay;
mod bottom_pane_view;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalImageAttachment {
    pub placeholder: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionBinding {
    /// Mention token text without the leading `$`.
    pub mention: String,
    /// Canonical mention target (for example `app://...` or absolute SKILL.md path).
    pub path: String,
}
mod chat_composer;
mod chat_composer_history;
mod command_popup;
mod composer_draft;
pub mod custom_prompt_view;
mod file_search_popup;
mod footer;
mod footer_tips;
mod list_selection_view;
mod prompt_args;
mod slash_commands;
pub use footer::CollaborationModeIndicator;
pub use list_selection_view::ColumnWidthMode;
pub(crate) use list_selection_view::ListSelectionView;
pub use list_selection_view::SelectionViewParams;
pub use list_selection_view::SideContentWidth;
pub use list_selection_view::popup_content_width;
pub use list_selection_view::side_by_side_layout_widths;
mod paste_burst;
mod pending_input_preview;
mod pending_process_approvals;
pub mod popup_consts;
mod scroll_state;
mod selection_popup_common;
mod textarea;
mod unified_exec_footer;

/// How long the "press again to quit" hint stays visible.
///
/// This is shared between:
/// - `ChatWidget`: arming the double-press quit shortcut.
/// - `BottomPane`/`ChatComposer`: rendering and expiring the footer hint.
///
/// Keeping a single value ensures Ctrl+C and Ctrl+D behave identically.
pub const QUIT_SHORTCUT_TIMEOUT: Duration = Duration::from_secs(1);

/// Whether Ctrl+C/Ctrl+D require a second press to quit.
///
/// This UX experiment was enabled by default, but requiring a double press to quit feels janky in
/// practice (especially for users accustomed to shells and other TUIs). Disable it for now while we
/// rethink a better quit/interrupt design.
pub const DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED: bool = false;

/// The result of offering a cancellation key to a bottom-pane surface.
///
/// This is primarily used for Ctrl+C routing: active views can consume the key to dismiss
/// themselves, and the caller can decide what higher-level action (if any) to take when the key is
/// not handled locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationEvent {
    Handled,
    NotHandled,
}

use crate::bottom_pane::prompt_args::parse_slash_name;
use chaos_ipc::custom_prompts::CustomPrompt;
pub use chat_composer::ChatComposer;
pub use chat_composer::ChatComposerConfig;
pub use chat_composer::InputResult;

use crate::status_indicator_widget::StatusDetailsCapitalization;
use crate::status_indicator_widget::StatusIndicatorWidget;
pub use list_selection_view::SelectionAction;
pub use list_selection_view::SelectionItem;

/// Pane displayed in the lower half of the chat UI.
///
/// This is the owning container for the prompt input (`ChatComposer`) and the view stack
/// (`BottomPaneView`). It performs local input routing and renders time-based hints, while leaving
/// process-level decisions (quit, interrupt, shutdown) to `ChatWidget`.
pub struct BottomPane {
    /// Composer is retained even when a BottomPaneView is displayed so the
    /// input state is retained when the view is closed.
    composer: ChatComposer,

    /// Stack of views displayed instead of the composer (e.g. popups/modals).
    view_stack: Vec<Box<dyn BottomPaneView>>,

    /// A key that closed a view/popup must not repeat into the surface underneath.
    suppressed_repeat_key: Option<KeyCode>,

    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,

    has_input_focus: bool,
    enhanced_keys_supported: bool,
    disable_paste_burst: bool,
    is_task_running: bool,
    esc_backtrack_hint: bool,
    animations_enabled: bool,

    /// Inline status indicator shown above the composer while a task is running.
    status: Option<StatusIndicatorWidget>,
    /// Unified exec session summary source.
    ///
    /// When a status row exists, this summary is mirrored inline in that row;
    /// when no status row exists, it renders as its own footer row.
    unified_exec_footer: UnifiedExecFooter,
    /// Preview of pending steers and queued drafts shown above the composer.
    pending_input_preview: PendingInputPreview,
    /// Inactive threads with pending approval requests.
    pending_process_approvals: PendingProcessApprovals,
    /// Approximate live model-token progress for the active turn.
    turn_progress_message: Option<String>,
    context_window_percent: Option<i64>,
    context_window_used_tokens: Option<i64>,
}

pub struct BottomPaneParams {
    pub app_event_tx: AppEventSender,
    pub frame_requester: FrameRequester,
    pub has_input_focus: bool,
    pub enhanced_keys_supported: bool,
    pub placeholder_text: String,
    pub disable_paste_burst: bool,
    pub animations_enabled: bool,
}

mod state;

#[cfg(test)]
pub(crate) mod tests;
