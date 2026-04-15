//! The chat composer is the bottom-pane text input state machine.
//!
//! It is responsible for:
//!
//! - Editing the input buffer (a [`TextArea`]), including placeholder "elements" for attachments.
//! - Routing keys to the active popup (slash commands, file search, skill/apps mentions).
//! - Promoting typed slash commands into atomic elements when the command name is completed.
//! - Handling submit vs newline on Enter.
//! - Turning raw key streams into explicit paste operations on platforms where terminals
//!   don't provide reliable bracketed paste (notably Windows).
//!
//! # Key Event Routing
//!
//! Most key handling goes through [`ChatComposer::handle_key_event`], which dispatches to a
//! popup-specific handler if a popup is visible and otherwise to
//! [`ChatComposer::handle_key_event_without_popup`]. After every handled key, we call
//! [`ChatComposer::sync_popups`] so UI state follows the latest buffer/cursor.
//!
//! # History Navigation (↑/↓)
//!
//! The Up/Down history path is managed by [`ChatComposerHistory`]. It merges:
//!
//! - Persistent cross-session history (text-only; no element ranges or attachments).
//! - Local in-session history (full text + text elements + local/remote image attachments).
//!
//! When recalling a local entry, the composer rehydrates text elements and both attachment kinds
//! (local image paths + remote image URLs).
//! When recalling a persistent entry, only the text is restored.
//! Recalled entries move the cursor to end-of-line so repeated Up/Down presses keep shell-like
//! history traversal semantics instead of dropping to column 0.
//!
//! # Submission and Prompt Expansion
//!
//! `Enter` submits immediately. `Tab` requests queuing while a task is running; if no task is
//! running, `Tab` submits just like Enter so input is never dropped.
//! `Tab` does not submit when entering a `!` shell command.
//!
//! On submit/queue paths, the composer:
//!
//! - Expands pending paste placeholders so element ranges align with the final text.
//! - Trims whitespace and rebases text elements accordingly.
//! - Expands `/prompts:` custom prompts (named or numeric args), preserving text elements.
//! - Prunes local attached images so only placeholders that survive expansion are sent.
//! - Preserves remote image URLs as separate attachments even when text is empty.
//!
//! When these paths clear the visible textarea after a successful submit or slash-command
//! dispatch, they intentionally preserve the textarea kill buffer. That lets users `Ctrl+K` part
//! of a draft, perform a composer action such as changing reasoning level, and then `Ctrl+Y` the
//! killed text back into the now-empty draft.
//!
//! The numeric auto-submit path used by the slash popup performs the same pending-paste expansion
//! and attachment pruning, and clears pending paste state on success.
//! Slash commands with arguments (like `/plan` and `/review`) reuse the same preparation path so
//! pasted content and text elements are preserved when extracting args.
//!
//! # Remote Image Rows (Up/Down/Delete)
//!
//! Remote image URLs are rendered as non-editable `[Image #N]` rows above the textarea (inside the
//! same composer block). These rows represent image attachments rehydrated from app-server/backtrack
//! history; TUI users can remove them, but cannot type into that row region.
//!
//! Keyboard behavior:
//!
//! - `Up` at textarea cursor `0` enters remote-row selection at the last remote image.
//! - `Up`/`Down` move selection between remote rows.
//! - `Down` on the last row clears selection and returns control to the textarea.
//! - `Delete`/`Backspace` remove the selected remote image row.
//!
//! Placeholder numbering is unified across remote and local images:
//!
//! - Remote rows occupy `[Image #1]..[Image #M]`.
//! - Local placeholders are offset after that range (`[Image #M+1]..`).
//! - Deleting a remote row relabels local placeholders to keep numbering contiguous.
//!
//! # Non-bracketed Paste Bursts
//!
//! On some terminals (especially on Windows), pastes arrive as a rapid sequence of
//! `KeyCode::Char` and `KeyCode::Enter` key events instead of a single paste event.
//!
//! To avoid misinterpreting these bursts as real typing (and to prevent transient UI effects like
//! shortcut overlays toggling on a pasted `?`), we feed "plain" character events into
//! [`PasteBurst`](super::paste_burst::PasteBurst), which buffers bursts and later flushes them
//! through [`ChatComposer::handle_paste`].
//!
//! The burst detector intentionally treats ASCII and non-ASCII differently:
//!
//! - ASCII: we briefly hold the first fast char (flicker suppression) until we know whether the
//!   stream is paste-like.
//! - non-ASCII: we do not hold the first char (IME input would feel dropped), but we still allow
//!   burst detection for actual paste streams.
//!
//! The burst detector can also be disabled (`disable_paste_burst`), which bypasses the state
//! machine and treats the key stream as normal typing. When toggling from enabled → disabled, the
//! composer flushes/clears any in-flight burst state so it cannot leak into subsequent input.
//!
//! For the detailed burst state machine, see `bin/console/src/bottom_pane/paste_burst.rs`.
//! For a narrative overview of the combined state machine, see `docs/tui-chat-composer.md`.
//!
//! # PasteBurst Integration Points
//!
//! The burst detector is consulted in a few specific places:
//!
//! - [`ChatComposer::handle_input_basic`]: flushes any due burst first, then intercepts plain char
//!   input to either buffer it or insert normally.
//! - [`ChatComposer::handle_non_ascii_char`]: handles the non-ASCII/IME path without holding the
//!   first char, while still allowing paste detection via retro-capture.
//! - [`ChatComposer::flush_paste_burst_if_due`]/[`ChatComposer::handle_paste_burst_flush`]: called
//!   from UI ticks to turn a pending burst into either an explicit paste (`handle_paste`) or a
//!   normal typed character.
//!
//! # Input Disabled Mode
//!
//! The composer can be temporarily read-only (`input_enabled = false`). In that mode it ignores
//! edits and renders a placeholder prompt instead of the editable textarea. This is part of the
//! overall state machine, since it affects which transitions are even possible from a given UI
//! state.
//!
mod event_handling;
mod history_integration;
mod mentions;
mod paste_handling;
mod popup_sync;
mod render;
mod submission;
mod types;
// Sibling modules (command_popup, footer, etc.) are accessed by submodules
// via `crate::bottom_pane::*` paths directly.
use submission::prompt_selection_action;
pub use types::ChatComposerConfig;
pub use types::InputResult;
use types::LARGE_PASTE_CHAR_THRESHOLD;
use types::PromptSelectionAction;
use types::PromptSelectionMode;
use types::user_input_too_large_message;

use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::has_ctrl_or_alt;
use crossterm::event::KeyCode;
use ratatui::text::Line;

use super::chat_composer_history::ChatComposerHistory;
use super::chat_composer_history::HistoryEntry;
use super::command_popup::CommandItem;
use super::command_popup::CommandPopup;
use super::file_search_popup::FileSearchPopup;
use super::footer::CollaborationModeIndicator;
use super::footer::FooterMode;
use super::footer::esc_hint_mode;
use super::footer::reset_mode_after_activity;
use super::footer::toggle_shortcut_mode;
use super::paste_burst::CharDecision;
use super::paste_burst::PasteBurst;
use super::slash_commands;
use super::slash_commands::BuiltinCommandFlags;
use crate::bottom_pane::paste_burst::FlushResult;
use crate::bottom_pane::prompt_args::expand_custom_prompt;
use crate::bottom_pane::prompt_args::expand_if_numeric_with_positional_args;
use crate::bottom_pane::prompt_args::parse_slash_name;
use crate::bottom_pane::prompt_args::prompt_argument_names;
use crate::bottom_pane::prompt_args::prompt_command_with_arg_placeholders;
use crate::bottom_pane::prompt_args::prompt_has_numeric_placeholders;
use crate::slash_command::SlashCommand;
use chaos_ipc::custom_prompts::CustomPrompt;
use chaos_ipc::custom_prompts::PROMPTS_CMD_PREFIX;
use chaos_ipc::models::local_image_label_text;
use chaos_ipc::user_input::ByteRange;
use chaos_ipc::user_input::MAX_USER_INPUT_TEXT_CHARS;
use chaos_ipc::user_input::TextElement;

use crate::app_event::ConnectorsSnapshot;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::MentionBinding;
use crate::bottom_pane::textarea::TextArea;
use crate::bottom_pane::textarea::TextAreaState;
use crate::history_cell;
use crate::tui::FrameRequester;
use chaos_locate::FileMatch;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

/// If the pasted content exceeds this number of characters, replace it with a
/// placeholder in the UI.

#[derive(Clone, Debug, PartialEq)]
pub(super) struct AttachedImage {
    pub(super) placeholder: String,
    pub(super) path: PathBuf,
}

pub struct ChatComposer {
    pub(super) textarea: TextArea,
    pub(super) textarea_state: RefCell<TextAreaState>,
    pub(super) active_popup: ActivePopup,
    pub(super) app_event_tx: AppEventSender,
    pub(super) history: ChatComposerHistory,
    pub(super) quit_shortcut_expires_at: Option<Instant>,
    pub(super) quit_shortcut_key: KeyBinding,
    pub(super) esc_backtrack_hint: bool,
    pub(super) use_shift_enter_hint: bool,
    pub(super) dismissed_file_popup_token: Option<String>,
    pub(super) current_file_query: Option<String>,
    pub(super) pending_pastes: Vec<(String, String)>,
    pub(super) large_paste_counters: HashMap<usize, usize>,
    pub(super) has_focus: bool,
    pub(super) frame_requester: Option<FrameRequester>,
    /// Invariant: attached images are labeled in vec order as
    /// `[Image #M+1]..[Image #N]`, where `M` is the number of remote images.
    pub(super) attached_images: Vec<AttachedImage>,
    pub(super) placeholder_text: String,
    pub(super) is_task_running: bool,
    /// When false, the composer is temporarily read-only (e.g. during sandbox setup).
    pub(super) input_enabled: bool,
    pub(super) input_disabled_placeholder: Option<String>,
    /// Non-bracketed paste burst tracker (see `bottom_pane/paste_burst.rs`).
    pub(super) paste_burst: PasteBurst,
    // When true, disables paste-burst logic and inserts characters immediately.
    pub(super) disable_paste_burst: bool,
    pub(super) custom_prompts: Vec<CustomPrompt>,
    pub(super) footer_mode: FooterMode,
    pub(super) footer_hint_override: Option<Vec<(String, String)>>,
    pub(super) remote_image_urls: Vec<String>,
    /// Tracks keyboard selection for the remote-image rows so Up/Down + Delete/Backspace
    /// can highlight and remove remote attachments from the composer UI.
    pub(super) selected_remote_image_index: Option<usize>,
    pub(super) footer_flash: Option<FooterFlash>,
    pub(super) context_window_percent: Option<i64>,
    pub(super) context_window_used_tokens: Option<i64>,
    pub(super) connectors_snapshot: Option<ConnectorsSnapshot>,
    pub(super) dismissed_mention_popup_token: Option<String>,
    pub(super) mention_bindings: HashMap<u64, ComposerMentionBinding>,
    pub(super) recent_submission_mention_bindings: Vec<MentionBinding>,
    pub(super) collaboration_modes_enabled: bool,
    pub(super) config: ChatComposerConfig,
    pub(super) collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    pub(super) connectors_enabled: bool,
    pub(super) personality_command_enabled: bool,
    pub(super) status_line_value: Option<Line<'static>>,
    pub(super) status_line_enabled: bool,
    // Agent label injected into the footer's contextual row when multi-agent mode is active.
    pub(super) active_agent_label: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct FooterFlash {
    pub(super) line: Line<'static>,
    pub(super) expires_at: Instant,
}

#[derive(Clone, Debug)]
pub(super) struct ComposerMentionBinding {
    pub(super) mention: String,
    pub(super) path: String,
}

/// Popup state – at most one can be visible at any time.
pub(super) enum ActivePopup {
    None,
    Command(CommandPopup),
    File(FileSearchPopup),
}

pub(super) const FOOTER_SPACING_HEIGHT: u16 = 0;

impl ChatComposer {
    fn builtin_command_flags(&self) -> BuiltinCommandFlags {
        BuiltinCommandFlags {
            collaboration_modes_enabled: self.collaboration_modes_enabled,
            connectors_enabled: self.connectors_enabled,
            personality_command_enabled: self.personality_command_enabled,
            allow_elevate_sandbox: false,
        }
    }

    pub fn new(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
    ) -> Self {
        Self::new_with_config(
            has_input_focus,
            app_event_tx,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
            ChatComposerConfig::default(),
        )
    }

    /// Construct a composer with explicit feature gating.
    ///
    /// This enables reuse in contexts like request-user-input where we want
    /// the same visuals and editing behavior without slash commands or popups.
    pub fn new_with_config(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
        config: ChatComposerConfig,
    ) -> Self {
        let use_shift_enter_hint = enhanced_keys_supported;

        let mut this = Self {
            textarea: TextArea::new(),
            textarea_state: RefCell::new(TextAreaState::default()),
            active_popup: ActivePopup::None,
            app_event_tx,
            history: ChatComposerHistory::new(),
            quit_shortcut_expires_at: None,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            esc_backtrack_hint: false,
            use_shift_enter_hint,
            dismissed_file_popup_token: None,
            current_file_query: None,
            pending_pastes: Vec::new(),
            large_paste_counters: HashMap::new(),
            has_focus: has_input_focus,
            frame_requester: None,
            attached_images: Vec::new(),
            placeholder_text,
            is_task_running: false,
            input_enabled: true,
            input_disabled_placeholder: None,
            paste_burst: PasteBurst::default(),
            disable_paste_burst: false,
            custom_prompts: Vec::new(),
            footer_mode: FooterMode::ComposerEmpty,
            footer_hint_override: None,
            remote_image_urls: Vec::new(),
            selected_remote_image_index: None,
            footer_flash: None,
            context_window_percent: None,
            context_window_used_tokens: None,
            connectors_snapshot: None,
            dismissed_mention_popup_token: None,
            mention_bindings: HashMap::new(),
            recent_submission_mention_bindings: Vec::new(),
            collaboration_modes_enabled: false,
            config,
            collaboration_mode_indicator: None,
            connectors_enabled: false,
            personality_command_enabled: false,
            status_line_value: None,
            status_line_enabled: false,
            active_agent_label: None,
        };
        // Apply configuration via the setter to keep side-effects centralized.
        this.set_disable_paste_burst(disable_paste_burst);
        this
    }

    pub fn set_frame_requester(&mut self, frame_requester: FrameRequester) {
        self.frame_requester = Some(frame_requester);
    }

    /// Toggle composer-side image paste handling.
    ///
    /// This only affects whether image-like paste content is converted into attachments; the
    /// `ChatWidget` layer still performs capability checks before images are submitted.
    pub fn set_image_paste_enabled(&mut self, enabled: bool) {
        self.config.image_paste_enabled = enabled;
    }

    pub fn set_connector_mentions(&mut self, connectors_snapshot: Option<ConnectorsSnapshot>) {
        self.connectors_snapshot = connectors_snapshot;
        self.sync_popups();
    }

    pub fn take_mention_bindings(&mut self) -> Vec<MentionBinding> {
        let elements = self.current_mention_elements();
        let mut ordered = Vec::new();
        for (id, mention) in elements {
            if let Some(binding) = self.mention_bindings.remove(&id)
                && binding.mention == mention
            {
                ordered.push(MentionBinding {
                    mention: binding.mention,
                    path: binding.path,
                });
            }
        }
        self.mention_bindings.clear();
        ordered
    }

    pub fn set_collaboration_modes_enabled(&mut self, enabled: bool) {
        self.collaboration_modes_enabled = enabled;
    }

    pub fn set_connectors_enabled(&mut self, enabled: bool) {
        self.connectors_enabled = enabled;
    }

    pub fn set_collaboration_mode_indicator(
        &mut self,
        indicator: Option<CollaborationModeIndicator>,
    ) {
        self.collaboration_mode_indicator = indicator;
    }

    pub fn set_personality_command_enabled(&mut self, enabled: bool) {
        self.personality_command_enabled = enabled;
    }

    /// Compatibility shim for tests that still toggle the removed steer mode flag.
    #[cfg(test)]
    pub fn set_steer_enabled(&mut self, _enabled: bool) {}
    /// Centralized feature gating keeps config checks out of call sites.
    fn popups_enabled(&self) -> bool {
        self.config.popups_enabled
    }

    fn slash_commands_enabled(&self) -> bool {
        self.config.slash_commands_enabled
    }

    fn image_paste_enabled(&self) -> bool {
        self.config.image_paste_enabled
    }

    /// Returns true if the composer currently contains no user-entered input.
    pub fn is_empty(&self) -> bool {
        self.textarea.is_empty()
            && self.attached_images.is_empty()
            && self.remote_image_urls.is_empty()
    }

    /// Override the footer hint items displayed beneath the composer. Passing
    /// `None` restores the default shortcut footer.
    pub fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.footer_hint_override = items;
    }

    pub fn set_remote_image_urls(&mut self, urls: Vec<String>) {
        self.remote_image_urls = urls;
        self.selected_remote_image_index = None;
        self.relabel_attached_images_and_update_placeholders();
        self.sync_popups();
    }

    pub fn remote_image_urls(&self) -> Vec<String> {
        self.remote_image_urls.clone()
    }

    pub fn take_remote_image_urls(&mut self) -> Vec<String> {
        let urls = std::mem::take(&mut self.remote_image_urls);
        self.selected_remote_image_index = None;
        self.relabel_attached_images_and_update_placeholders();
        self.sync_popups();
        urls
    }

    #[cfg(test)]
    pub fn show_footer_flash(&mut self, line: Line<'static>, duration: Duration) {
        let expires_at = Instant::now()
            .checked_add(duration)
            .unwrap_or_else(Instant::now);
        self.footer_flash = Some(FooterFlash { line, expires_at });
    }

    pub fn footer_flash_visible(&self) -> bool {
        self.footer_flash
            .as_ref()
            .is_some_and(|flash| Instant::now() < flash.expires_at)
    }

    /// Replace the entire composer content with `text` and reset cursor.
    ///
    /// This is the "fresh draft" path: it clears pending paste payloads and
    /// mention link targets. Callers restoring a previously submitted draft
    /// that must keep `$name -> path` resolution should use
    /// [`Self::set_text_content_with_mention_bindings`] instead.
    pub fn set_text_content(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        self.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            Vec::new(),
        );
    }

    /// Replace the entire composer content while restoring mention link targets.
    ///
    /// Mention popup insertion stores both visible text (for example `$file`)
    /// and hidden mention bindings used to resolve the canonical target during
    /// submission. Use this method when restoring an interrupted or blocked
    /// draft; if callers restore only text and images, mentions can appear
    /// intact to users while resolving to the wrong target or dropping on
    /// retry.
    ///
    /// This helper intentionally places the cursor at the start of the restored text. Callers
    /// that need end-of-line restore behavior (for example shell-style history recall) should call
    /// [`Self::move_cursor_to_end`] after this method.
    pub fn set_text_content_with_mention_bindings(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
        mention_bindings: Vec<MentionBinding>,
    ) {
        // Clear any existing content, placeholders, and attachments first.
        self.textarea.set_text_clearing_elements("");
        self.pending_pastes.clear();
        self.attached_images.clear();
        self.mention_bindings.clear();

        self.textarea.set_text_with_elements(&text, &text_elements);

        for (idx, path) in local_image_paths.into_iter().enumerate() {
            let placeholder = local_image_label_text(self.remote_image_urls.len() + idx + 1);
            self.attached_images
                .push(AttachedImage { placeholder, path });
        }

        self.bind_mentions_from_snapshot(mention_bindings);
        self.relabel_attached_images_and_update_placeholders();
        self.selected_remote_image_index = None;
        self.textarea.set_cursor(/*pos*/ 0);
        self.sync_popups();
    }

    /// Update the placeholder text without changing input enablement.
    pub fn set_placeholder_text(&mut self, placeholder: String) {
        self.placeholder_text = placeholder;
    }

    /// Move the cursor to the end of the current text buffer.
    pub fn move_cursor_to_end(&mut self) {
        self.textarea.set_cursor(self.textarea.text().len());
        self.sync_popups();
    }

    /// Get the current composer text.
    pub fn current_text(&self) -> String {
        self.textarea.text().to_string()
    }

    pub fn text_elements(&self) -> Vec<TextElement> {
        self.textarea.text_elements()
    }

    #[cfg(test)]
    pub fn local_image_paths(&self) -> Vec<PathBuf> {
        self.attached_images
            .iter()
            .map(|img| img.path.clone())
            .collect()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn status_line_text(&self) -> Option<String> {
        self.status_line_value.as_ref().map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
    }

    pub fn local_images(&self) -> Vec<LocalImageAttachment> {
        self.attached_images
            .iter()
            .map(|img| LocalImageAttachment {
                placeholder: img.placeholder.clone(),
                path: img.path.clone(),
            })
            .collect()
    }

    pub fn mention_bindings(&self) -> Vec<MentionBinding> {
        self.snapshot_mention_bindings()
    }

    pub fn take_recent_submission_mention_bindings(&mut self) -> Vec<MentionBinding> {
        std::mem::take(&mut self.recent_submission_mention_bindings)
    }

    /// Returns whether the composer is currently in any paste-burst related transient state.
    ///
    /// This includes actively buffering, having a non-empty burst buffer, or holding the first
    /// ASCII char for flicker suppression.
    pub fn is_in_paste_burst(&self) -> bool {
        self.paste_burst.is_active()
    }

    /// Returns a delay that reliably exceeds the paste-burst timing threshold.
    ///
    /// Use this in tests to avoid boundary flakiness around the `PasteBurst` timeout.
    pub fn recommended_paste_flush_delay() -> Duration {
        PasteBurst::recommended_flush_delay()
    }

    /// Integrate results from an asynchronous file search.
    pub fn on_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        // Only apply if user is still editing a token starting with `query`.
        let current_opt = Self::current_at_token(&self.textarea);
        let Some(current_token) = current_opt else {
            return;
        };

        if !current_token.starts_with(&query) {
            return;
        }

        if let ActivePopup::File(popup) = &mut self.active_popup {
            popup.set_matches(&query, matches);
        }
    }

    /// Show the transient "press again to quit" hint for `key`.
    ///
    /// The owner (`BottomPane`/`ChatWidget`) is responsible for scheduling a
    /// redraw after [`super::QUIT_SHORTCUT_TIMEOUT`] so the hint can disappear
    /// even when the UI is otherwise idle.
    pub fn show_quit_shortcut_hint(&mut self, key: KeyBinding, has_focus: bool) {
        self.quit_shortcut_expires_at = Instant::now()
            .checked_add(super::QUIT_SHORTCUT_TIMEOUT)
            .or_else(|| Some(Instant::now()));
        self.quit_shortcut_key = key;
        self.footer_mode = FooterMode::QuitShortcutReminder;
        self.set_has_focus(has_focus);
    }

    /// Clear the "press again to quit" hint immediately.
    pub fn clear_quit_shortcut_hint(&mut self, has_focus: bool) {
        self.quit_shortcut_expires_at = None;
        self.footer_mode = reset_mode_after_activity(self.footer_mode);
        self.set_has_focus(has_focus);
    }

    /// Whether the quit shortcut hint should currently be shown.
    ///
    /// This is time-based rather than event-based: it may become false without
    /// any additional user input, so the UI schedules a redraw when the hint
    /// expires.
    pub fn quit_shortcut_hint_visible(&self) -> bool {
        self.quit_shortcut_expires_at
            .is_some_and(|expires_at| Instant::now() < expires_at)
    }

    pub fn insert_str(&mut self, text: &str) {
        self.textarea.insert_str(text);
        self.sync_popups();
    }

    /// Return true if either the slash-command popup or the file-search popup is active.
    pub fn popup_active(&self) -> bool {
        !matches!(self.active_popup, ActivePopup::None)
    }

    fn set_has_focus(&mut self, has_focus: bool) {
        self.has_focus = has_focus;
    }

    pub fn set_input_enabled(&mut self, enabled: bool, placeholder: Option<String>) {
        self.input_enabled = enabled;
        self.input_disabled_placeholder = if enabled { None } else { placeholder };

        // Avoid leaving interactive popups open while input is blocked.
        if !enabled && !matches!(self.active_popup, ActivePopup::None) {
            self.active_popup = ActivePopup::None;
        }
    }

    pub fn set_task_running(&mut self, running: bool) {
        self.is_task_running = running;
    }

    pub fn set_context_window(&mut self, percent: Option<i64>, used_tokens: Option<i64>) {
        if self.context_window_percent == percent && self.context_window_used_tokens == used_tokens
        {
            return;
        }
        self.context_window_percent = percent;
        self.context_window_used_tokens = used_tokens;
    }

    pub fn set_esc_backtrack_hint(&mut self, show: bool) {
        self.esc_backtrack_hint = show;
        if show {
            self.footer_mode = esc_hint_mode(self.footer_mode, self.is_task_running);
        } else {
            self.footer_mode = reset_mode_after_activity(self.footer_mode);
        }
    }

    pub fn set_status_line(&mut self, status_line: Option<Line<'static>>) -> bool {
        if self.status_line_value == status_line {
            return false;
        }
        self.status_line_value = status_line;
        true
    }

    pub fn set_status_line_enabled(&mut self, enabled: bool) -> bool {
        if self.status_line_enabled == enabled {
            return false;
        }
        self.status_line_enabled = enabled;
        true
    }

    /// Replaces the contextual footer label for the currently viewed agent.
    ///
    /// Returning `false` means the value was unchanged, so callers can skip redraw work. This
    /// field is intentionally just cached presentation state; `ChatComposer` does not infer which
    /// thread is active on its own.
    pub fn set_active_agent_label(&mut self, active_agent_label: Option<String>) -> bool {
        if self.active_agent_label == active_agent_label {
            return false;
        }
        self.active_agent_label = active_agent_label;
        true
    }
}

impl ChatComposer {}

#[cfg(test)]
#[path = "chat_composer/chat_composer_tests.rs"]
mod tests;
