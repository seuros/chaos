use super::{
    AgentNavigationDirection, App, ExternalEditorState, HypertileEvent, KeyCode, KeyEvent,
    KeyEventKind, PaneId, TuiEvent, keychord_from_crossterm, next_agent_shortcut_matches,
    previous_agent_shortcut_matches, tui,
};

impl App {
    pub(super) async fn handle_key_event(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        // Some terminals, especially on macOS, encode Option+Left/Right as Option+b/f unless
        // enhanced keyboard reporting is available. We only treat those word-motion fallbacks as
        // agent-switch shortcuts when the composer is empty so we never steal the expected
        // editing behavior for moving across words inside a draft.
        let allow_agent_word_motion_fallback = !self.enhanced_keys_supported
            && self.chat_widget.composer_text_with_pending().is_empty();
        if self.overlay.is_none()
            && self.chat_widget.no_modal_or_popup_active()
            // Alt+Left/Right are also natural word-motion keys in the composer. Keep agent
            // fast-switch available only once the draft is empty so editing behavior wins whenever
            // there is text on screen.
            && self.chat_widget.composer_text_with_pending().is_empty()
            && previous_agent_shortcut_matches(key_event, allow_agent_word_motion_fallback)
        {
            if let Some(process_id) = self.agent_navigation.adjacent_process_id(
                self.current_displayed_process_id(),
                AgentNavigationDirection::Previous,
            ) {
                let _ = self.select_agent_process(tui, process_id).await;
            }
            return;
        }
        if self.overlay.is_none()
            && self.chat_widget.no_modal_or_popup_active()
            // Mirror the previous-agent rule above: empty drafts may use these keys for process
            // switching, but non-empty drafts keep them for expected word-wise cursor motion.
            && self.chat_widget.composer_text_with_pending().is_empty()
            && next_agent_shortcut_matches(key_event, allow_agent_word_motion_fallback)
        {
            if let Some(process_id) = self.agent_navigation.adjacent_process_id(
                self.current_displayed_process_id(),
                AgentNavigationDirection::Next,
            ) {
                let _ = self.select_agent_process(tui, process_id).await;
            }
            return;
        }

        // Tiling shortcuts — only active when multiple panes are open.
        if !self.tile_manager.is_single_pane()
            && self.overlay.is_none()
            && self.chat_widget.no_modal_or_popup_active()
            && key_event.kind == KeyEventKind::Press
        {
            use crossterm::event::KeyModifiers;
            use ratatui::layout::Direction;
            use ratatui_hypertile::HypertileAction;
            use ratatui_hypertile::Towards;

            let alt = key_event.modifiers.contains(KeyModifiers::ALT);
            let alt_shift = key_event
                .modifiers
                .contains(KeyModifiers::ALT | KeyModifiers::SHIFT);

            let tiling_action = match (key_event.code, alt, alt_shift) {
                // Alt+h/j/k/l — focus direction
                (KeyCode::Char('h'), true, false) => Some(HypertileAction::FocusDirection {
                    direction: Direction::Horizontal,
                    towards: Towards::Start,
                }),
                (KeyCode::Char('l'), true, false) => Some(HypertileAction::FocusDirection {
                    direction: Direction::Horizontal,
                    towards: Towards::End,
                }),
                (KeyCode::Char('k'), true, false) => Some(HypertileAction::FocusDirection {
                    direction: Direction::Vertical,
                    towards: Towards::Start,
                }),
                (KeyCode::Char('j'), true, false) => Some(HypertileAction::FocusDirection {
                    direction: Direction::Vertical,
                    towards: Towards::End,
                }),
                // Alt+Shift+H/J/K/L — resize focused pane
                (KeyCode::Char('H'), _, true) => {
                    Some(HypertileAction::ResizeFocused { delta: -0.05 })
                }
                (KeyCode::Char('L'), _, true) => {
                    Some(HypertileAction::ResizeFocused { delta: 0.05 })
                }
                (KeyCode::Char('K'), _, true) => {
                    Some(HypertileAction::ResizeFocused { delta: -0.05 })
                }
                (KeyCode::Char('J'), _, true) => {
                    Some(HypertileAction::ResizeFocused { delta: 0.05 })
                }
                // Alt+q — close auxiliary panes (tries focused first, then last opened)
                (KeyCode::Char('q'), true, false) => {
                    // Try closing focused first; if it's Chat, close the last auxiliary.
                    if self.tile_manager.close_focused().is_none() {
                        self.tile_manager.close_last_auxiliary();
                    }
                    tui.frame_requester().schedule_frame();
                    return;
                }
                // Alt+w — close all auxiliary panes, return to single chat
                (KeyCode::Char('w'), true, false) => {
                    self.tile_manager.close_all_auxiliary();
                    tui.frame_requester().schedule_frame();
                    return;
                }
                // Alt+Enter — cycle focus
                (KeyCode::Enter, true, false) => Some(HypertileAction::FocusNext),
                _ => None,
            };

            if let Some(action) = tiling_action {
                self.tile_manager.apply_action(action);
                tui.frame_requester().schedule_frame();
                return;
            }
        }

        // ── Global shortcuts ─────────────────────────────────────────
        // These work regardless of which pane is focused.
        match key_event {
            KeyEvent {
                code: KeyCode::Char('o'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.toggle_log_panel(tui).await;
                return;
            }
            KeyEvent {
                code: KeyCode::PageUp,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.open_transcript_overlay(tui, Some(TuiEvent::Key(key_event)));
                return;
            }
            KeyEvent {
                code: KeyCode::PageDown,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.open_transcript_overlay(tui, Some(TuiEvent::Key(key_event)));
                return;
            }
            KeyEvent {
                code: KeyCode::Home,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.open_transcript_overlay(tui, Some(TuiEvent::Key(key_event)));
                return;
            }
            KeyEvent {
                code: KeyCode::End,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.open_transcript_overlay(tui, Some(TuiEvent::Key(key_event)));
                return;
            }
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                self.open_transcript_overlay(tui, None);
                return;
            }
            KeyEvent {
                code: KeyCode::Char('l'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                if !self.chat_widget.can_run_ctrl_l_clear_now() {
                    return;
                }
                if let Err(err) = self.clear_terminal_ui(tui, /*redraw_header*/ false) {
                    tracing::warn!(error = %err, "failed to clear terminal UI");
                    self.chat_widget
                        .add_error_message(format!("Failed to clear terminal UI: {err}"));
                } else {
                    self.reset_app_ui_state_after_clear();
                    self.queue_clear_ui_header(tui);
                    tui.frame_requester().schedule_frame();
                }
                return;
            }
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                if self.overlay.is_none()
                    && self.chat_widget.can_launch_external_editor()
                    && self.chat_widget.external_editor_state() == ExternalEditorState::Closed
                {
                    self.request_external_editor_launch(tui);
                }
                return;
            }
            _ => {}
        }

        // ── Focused-pane local input ────────────────────────────────
        // When an auxiliary pane is focused, give it the key first.
        // If the pane ignores the key we swallow it — unmodified keys
        // must never leak into the chat composer.
        if let Some(focused) = self.tile_manager.focused()
            && focused != PaneId::ROOT
        {
            if matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                && let Some(chord) = keychord_from_crossterm(key_event)
                && let Some(plugin) = self.tile_manager.plugin_mut(focused)
            {
                let _ = plugin.on_event(&HypertileEvent::Key(chord));
            }

            let should_close = self.tool_list_close.replace(false)
                || (key_event.kind == KeyEventKind::Press && key_event.code == KeyCode::Esc);

            if should_close {
                self.tile_manager.close_pane(focused);
            }

            tui.frame_requester().schedule_frame();
            return;
        }

        // ── Chat pane input ─────────────────────────────────────────
        // Only reached when Chat (ROOT) is focused.
        match key_event {
            // Esc primes/advances backtracking only in normal (not working) mode
            // with the composer focused and empty. In any other state, forward
            // Esc so the active UI (e.g. status indicator, modals, popups)
            // handles it.
            KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.chat_widget.is_normal_backtrack_mode()
                && self.chat_widget.composer_is_empty() =>
            {
                self.handle_backtrack_esc_key(tui);
            }
            KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                self.chat_widget.handle_key_event(key_event);
            }
            // Enter confirms backtrack when primed + count > 0. Otherwise pass to widget.
            KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            } if self.backtrack.primed
                && self.backtrack.nth_user_message != usize::MAX
                && self.chat_widget.composer_is_empty() =>
            {
                if let Some(selection) = self.confirm_backtrack_from_main() {
                    self.apply_backtrack_selection(tui, selection);
                }
            }
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                // Any non-Esc key press should cancel a primed backtrack.
                if key_event.code != KeyCode::Esc && self.backtrack.primed {
                    self.reset_backtrack_state();
                }
                self.chat_widget.handle_key_event(key_event);
            }
            _ => {
                self.chat_widget.handle_key_event(key_event);
            }
        };
    }
}
