use crate::key_hint::KeyBinding;
use ratatui::text::Line;

/// The rendering inputs for the footer area under the composer.
///
/// Callers are expected to construct `FooterProps` from higher-level state (`ChatComposer`,
/// `BottomPane`, and `ChatWidget`) and pass it to the footer render helpers
/// (`render_footer_from_props` or the single-line collapse logic). The footer
/// treats these values as authoritative and does not attempt to infer missing
/// state (for example, it does not query whether a task is running).
#[derive(Clone, Debug)]
pub struct FooterProps {
    pub mode: FooterMode,
    pub esc_backtrack_hint: bool,
    pub use_shift_enter_hint: bool,
    pub is_task_running: bool,
    pub collaboration_modes_enabled: bool,
    /// Which key the user must press again to quit.
    ///
    /// This is rendered when `mode` is `FooterMode::QuitShortcutReminder`.
    pub quit_shortcut_key: KeyBinding,
    pub context_window_percent: Option<i64>,
    pub context_window_used_tokens: Option<i64>,
    pub status_line_value: Option<Line<'static>>,
    pub status_line_enabled: bool,
    /// Active thread label shown when the footer is rendering contextual information instead of an
    /// instructional hint.
    ///
    /// When both this label and the configured status line are available, they are rendered on the
    /// same row separated by ` · `.
    pub active_agent_label: Option<String>,
}

/// Selects which footer content is rendered.
///
/// The current mode is owned by `ChatComposer`, which may override it based on transient state
/// (for example, showing `QuitShortcutReminder` only while its timer is active).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FooterMode {
    /// Transient "press again to quit" reminder (Ctrl+C/Ctrl+D).
    QuitShortcutReminder,
    /// Multi-line shortcut overlay shown after pressing `?`.
    ShortcutOverlay,
    /// Transient "press Esc again" hint shown after the first Esc while idle.
    EscHint,
    /// Base single-line footer when the composer is empty.
    ComposerEmpty,
    /// Base single-line footer when the composer contains a draft.
    ///
    /// The shortcuts hint is suppressed here; when a task is running, this
    /// mode can show the queue hint instead.
    ComposerHasDraft,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollaborationModeIndicator {
    Plan,
    #[allow(dead_code)] // Hidden by current mode filtering; kept for future UI re-enablement.
    PairProgramming,
    #[allow(dead_code)] // Hidden by current mode filtering; kept for future UI re-enablement.
    Execute,
}

/// Internal state for shortcut overlay rendering.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShortcutsState {
    pub use_shift_enter_hint: bool,
    pub esc_backtrack_hint: bool,
    pub collaboration_modes_enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SummaryHintKind {
    None,
    Shortcuts,
    QueueMessage,
    QueueShort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LeftSideState {
    pub hint: SummaryHintKind,
    pub show_cycle_hint: bool,
}

pub enum SummaryLeft {
    Default,
    Custom(Line<'static>),
    None,
}
