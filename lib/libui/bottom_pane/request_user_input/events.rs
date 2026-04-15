use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;

use crate::app_event::AppEvent;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;

use chaos_ipc::protocol::Op;
use chaos_ipc::user_input::TextElement;

use super::ComposerDraft;
use super::Focus;
use super::RequestUserInputOverlay;

impl RequestUserInputOverlay {
    pub(super) fn handle_composer_input_result(&mut self, result: InputResult) -> bool {
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            }
            | InputResult::Queued {
                text,
                text_elements,
            } => {
                if self.has_options()
                    && matches!(self.focus, Focus::Notes)
                    && !text.trim().is_empty()
                {
                    let options_len = self.options_len();
                    if let Some(answer) = self.current_answer_mut() {
                        answer.options_state.clamp_selection(options_len);
                    }
                }
                if self.has_options() {
                    if let Some(answer) = self.current_answer_mut() {
                        answer.answer_committed = true;
                    }
                } else if let Some(answer) = self.current_answer_mut() {
                    answer.answer_committed = !text.trim().is_empty();
                }
                let draft_override = self.pending_submission_draft.take();
                if let Some(draft) = draft_override {
                    self.apply_submission_draft(draft);
                } else {
                    self.apply_submission_to_draft(text, text_elements);
                }
                self.go_next_or_submit();
                true
            }
            _ => false,
        }
    }

    pub(super) fn handle_confirm_unanswered_key_event(&mut self, key_event: KeyEvent) {
        let Some(state) = self.confirm_unanswered.as_mut() else {
            return;
        };

        match key_event.code {
            KeyCode::Esc | KeyCode::Backspace => {
                self.close_unanswered_confirmation();
                if let Some(idx) = self.first_unanswered_index() {
                    self.jump_to_question(idx);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.move_up_wrap(/*len*/ 2);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.move_down_wrap(/*len*/ 2);
            }
            KeyCode::Enter => {
                let selected = state.selected_idx.unwrap_or(0);
                self.close_unanswered_confirmation();
                if selected == 0 {
                    self.submit_answers();
                } else if let Some(idx) = self.first_unanswered_index() {
                    self.jump_to_question(idx);
                }
            }
            KeyCode::Char('1') | KeyCode::Char('2') => {
                let idx = if matches!(key_event.code, KeyCode::Char('1')) {
                    0
                } else {
                    1
                };
                state.selected_idx = Some(idx);
            }
            _ => {}
        }
    }

    pub(super) fn apply_submission_to_draft(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
    ) {
        let local_image_paths = self
            .composer
            .local_images()
            .into_iter()
            .map(|img| img.path)
            .collect::<Vec<_>>();
        if let Some(answer) = self.current_answer_mut() {
            answer.draft = ComposerDraft {
                text: text.clone(),
                text_elements: text_elements.clone(),
                local_image_paths: local_image_paths.clone(),
                pending_pastes: Vec::new(),
            };
        }
        self.composer
            .set_text_content(text, text_elements, local_image_paths);
        self.composer.move_cursor_to_end();
        self.composer.set_footer_hint_override(Some(Vec::new()));
    }

    pub(super) fn apply_submission_draft(&mut self, draft: ComposerDraft) {
        if let Some(answer) = self.current_answer_mut() {
            answer.draft = draft.clone();
        }
        self.composer
            .set_text_content(draft.text, draft.text_elements, draft.local_image_paths);
        self.composer.set_pending_pastes(draft.pending_pastes);
        self.composer.move_cursor_to_end();
        self.composer.set_footer_hint_override(Some(Vec::new()));
    }
}

impl BottomPaneView for RequestUserInputOverlay {
    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.confirm_unanswered_active() {
            self.handle_confirm_unanswered_key_event(key_event);
            return;
        }

        if matches!(key_event.code, KeyCode::Esc) {
            if self.has_options() && self.notes_ui_visible() {
                self.clear_notes_and_focus_options();
                return;
            }
            // TODO: Emit interrupted request_user_input results (including committed answers)
            // once core supports persisting them reliably without follow-up turn issues.
            self.app_event_tx.send(AppEvent::ChaosOp(Op::Interrupt));
            self.done = true;
            return;
        }

        // Question navigation is always available.
        match key_event {
            KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::PageUp,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_question(/*next*/ false);
                return;
            }
            KeyEvent {
                code: KeyCode::PageDown,
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.move_question(/*next*/ true);
                return;
            }
            KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.has_options() && matches!(self.focus, Focus::Options) => {
                self.move_question(/*next*/ false);
                return;
            }
            KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.has_options() && matches!(self.focus, Focus::Options) => {
                self.move_question(/*next*/ false);
                return;
            }
            KeyEvent {
                code: KeyCode::Char('l'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.has_options() && matches!(self.focus, Focus::Options) => {
                self.move_question(/*next*/ true);
                return;
            }
            KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.has_options() && matches!(self.focus, Focus::Options) => {
                self.move_question(/*next*/ true);
                return;
            }
            _ => {}
        }

        match self.focus {
            Focus::Options => {
                let options_len = self.options_len();
                // Keep selection synchronized as the user moves.
                match key_event.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        let moved = if let Some(answer) = self.current_answer_mut() {
                            answer.options_state.move_up_wrap(options_len);
                            answer.answer_committed = false;
                            true
                        } else {
                            false
                        };
                        if moved {
                            self.sync_composer_placeholder();
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let moved = if let Some(answer) = self.current_answer_mut() {
                            answer.options_state.move_down_wrap(options_len);
                            answer.answer_committed = false;
                            true
                        } else {
                            false
                        };
                        if moved {
                            self.sync_composer_placeholder();
                        }
                    }
                    KeyCode::Char(' ') => {
                        self.select_current_option(/*committed*/ true);
                    }
                    KeyCode::Backspace | KeyCode::Delete => {
                        self.clear_selection();
                    }
                    KeyCode::Tab => {
                        if self.selected_option_index().is_some() {
                            self.focus = Focus::Notes;
                            self.ensure_selected_for_notes();
                        }
                    }
                    KeyCode::Enter => {
                        let has_selection = self.selected_option_index().is_some();
                        if has_selection {
                            self.select_current_option(/*committed*/ true);
                        }
                        self.go_next_or_submit();
                    }
                    KeyCode::Char(ch) => {
                        if let Some(option_idx) = self.option_index_for_digit(ch) {
                            if let Some(answer) = self.current_answer_mut() {
                                answer.options_state.selected_idx = Some(option_idx);
                            }
                            self.select_current_option(/*committed*/ true);
                            self.go_next_or_submit();
                        }
                    }
                    _ => {}
                }
            }
            Focus::Notes => {
                let notes_empty = self.composer.current_text_with_pending().trim().is_empty();
                if self.has_options() && matches!(key_event.code, KeyCode::Tab) {
                    self.clear_notes_and_focus_options();
                    return;
                }
                if self.has_options() && matches!(key_event.code, KeyCode::Backspace) && notes_empty
                {
                    self.save_current_draft();
                    if let Some(answer) = self.current_answer_mut() {
                        answer.notes_visible = false;
                    }
                    self.focus = Focus::Options;
                    self.sync_composer_placeholder();
                    return;
                }
                if matches!(key_event.code, KeyCode::Enter) {
                    self.ensure_selected_for_notes();
                    self.pending_submission_draft = Some(self.capture_composer_draft());
                    let (result, _) = self.composer.handle_key_event(key_event);
                    if !self.handle_composer_input_result(result) {
                        self.pending_submission_draft = None;
                        if self.has_options() {
                            self.select_current_option(/*committed*/ true);
                        }
                        self.go_next_or_submit();
                    }
                    return;
                }
                if self.has_options() && matches!(key_event.code, KeyCode::Up | KeyCode::Down) {
                    let options_len = self.options_len();
                    match key_event.code {
                        KeyCode::Up => {
                            let moved = if let Some(answer) = self.current_answer_mut() {
                                answer.options_state.move_up_wrap(options_len);
                                answer.answer_committed = false;
                                true
                            } else {
                                false
                            };
                            if moved {
                                self.sync_composer_placeholder();
                            }
                        }
                        KeyCode::Down => {
                            let moved = if let Some(answer) = self.current_answer_mut() {
                                answer.options_state.move_down_wrap(options_len);
                                answer.answer_committed = false;
                                true
                            } else {
                                false
                            };
                            if moved {
                                self.sync_composer_placeholder();
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                self.ensure_selected_for_notes();
                if matches!(
                    key_event.code,
                    KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
                ) && let Some(answer) = self.current_answer_mut()
                {
                    answer.answer_committed = false;
                }
                let before = self.capture_composer_draft();
                let (result, _) = self.composer.handle_key_event(key_event);
                let submitted = self.handle_composer_input_result(result);
                if !submitted {
                    let after = self.capture_composer_draft();
                    if before != after
                        && let Some(answer) = self.current_answer_mut()
                    {
                        answer.answer_committed = false;
                    }
                }
            }
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if self.confirm_unanswered_active() {
            self.close_unanswered_confirmation();
            // TODO: Emit interrupted request_user_input results (including committed answers)
            // once core supports persisting them reliably without follow-up turn issues.
            self.app_event_tx.send(AppEvent::ChaosOp(Op::Interrupt));
            self.done = true;
            return CancellationEvent::Handled;
        }
        if self.focus_is_notes() && !self.composer.current_text_with_pending().is_empty() {
            self.clear_notes_draft();
            return CancellationEvent::Handled;
        }

        // TODO: Emit interrupted request_user_input results (including committed answers)
        // once core supports persisting them reliably without follow-up turn issues.
        self.app_event_tx.send(AppEvent::ChaosOp(Op::Interrupt));
        self.done = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.done
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if pasted.is_empty() {
            return false;
        }
        if matches!(self.focus, Focus::Options) {
            // Treat pastes the same as typing: switch into notes.
            self.focus = Focus::Notes;
        }
        self.ensure_selected_for_notes();
        if let Some(answer) = self.current_answer_mut() {
            answer.answer_committed = false;
        }
        self.composer.handle_paste(pasted)
    }

    fn flush_paste_burst_if_due(&mut self) -> bool {
        self.composer.flush_paste_burst_if_due()
    }

    fn is_in_paste_burst(&self) -> bool {
        self.composer.is_in_paste_burst()
    }

    fn try_consume_user_input_request(
        &mut self,
        request: chaos_ipc::request_user_input::RequestUserInputEvent,
    ) -> Option<chaos_ipc::request_user_input::RequestUserInputEvent> {
        self.queue.push_back(request);
        None
    }
}
