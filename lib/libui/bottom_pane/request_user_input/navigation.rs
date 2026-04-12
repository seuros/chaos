use std::collections::HashMap;

use crate::app_event::AppEvent;
use crate::bottom_pane::scroll_state::ScrollState;
use crate::history_cell;

use chaos_ipc::protocol::Op;
use chaos_ipc::request_user_input::RequestUserInputAnswer;
use chaos_ipc::request_user_input::RequestUserInputResponse;

use super::ComposerDraft;
use super::Focus;
use super::RequestUserInputOverlay;

impl RequestUserInputOverlay {
    /// Move to the next/previous question, wrapping in either direction.
    pub(super) fn move_question(&mut self, next: bool) {
        let len = self.question_count();
        if len == 0 {
            return;
        }
        self.save_current_draft();
        let offset = if next { 1 } else { len.saturating_sub(1) };
        self.current_idx = (self.current_idx + offset) % len;
        self.restore_current_draft();
        self.ensure_focus_available();
    }

    pub(super) fn jump_to_question(&mut self, idx: usize) {
        if idx >= self.question_count() {
            return;
        }
        self.save_current_draft();
        self.current_idx = idx;
        self.restore_current_draft();
        self.ensure_focus_available();
    }

    /// Synchronize selection state to the currently focused option.
    pub(super) fn select_current_option(&mut self, committed: bool) {
        if !self.has_options() {
            return;
        }
        let options_len = self.options_len();
        let updated = if let Some(answer) = self.current_answer_mut() {
            answer.options_state.clamp_selection(options_len);
            answer.answer_committed = committed;
            true
        } else {
            false
        };
        if updated {
            self.sync_composer_placeholder();
        }
    }

    /// Clear the current option selection and hide notes when empty.
    pub(super) fn clear_selection(&mut self) {
        if !self.has_options() {
            return;
        }
        if let Some(answer) = self.current_answer_mut() {
            answer.options_state.reset();
            answer.draft = ComposerDraft::default();
            answer.answer_committed = false;
            answer.notes_visible = false;
        }
        self.pending_submission_draft = None;
        self.composer
            .set_text_content(String::new(), Vec::new(), Vec::new());
        self.composer.move_cursor_to_end();
        self.sync_composer_placeholder();
    }

    pub(super) fn clear_notes_and_focus_options(&mut self) {
        if !self.has_options() {
            return;
        }
        if let Some(answer) = self.current_answer_mut() {
            answer.draft = ComposerDraft::default();
            answer.answer_committed = false;
            answer.notes_visible = false;
        }
        self.pending_submission_draft = None;
        self.composer
            .set_text_content(String::new(), Vec::new(), Vec::new());
        self.composer.move_cursor_to_end();
        self.focus = Focus::Options;
        self.sync_composer_placeholder();
    }

    /// Ensure there is a selection before allowing notes entry.
    pub(super) fn ensure_selected_for_notes(&mut self) {
        if let Some(answer) = self.current_answer_mut() {
            answer.notes_visible = true;
        }
        self.sync_composer_placeholder();
    }

    /// Advance to next question, or submit when on the last one.
    pub(super) fn go_next_or_submit(&mut self) {
        if self.current_index() + 1 >= self.question_count() {
            self.save_current_draft();
            if self.unanswered_count() > 0 {
                self.open_unanswered_confirmation();
            } else {
                self.submit_answers();
            }
        } else {
            self.move_question(/*next*/ true);
        }
    }

    /// Build the response payload and dispatch it to the app.
    pub(super) fn submit_answers(&mut self) {
        self.confirm_unanswered = None;
        self.save_current_draft();
        let mut answers = HashMap::new();
        for (idx, question) in self.request.questions.iter().enumerate() {
            let answer_state = &self.answers[idx];
            let options = question.options.as_ref();
            // For option questions we may still produce no selection.
            let selected_idx =
                if options.is_some_and(|opts| !opts.is_empty()) && answer_state.answer_committed {
                    answer_state.options_state.selected_idx
                } else {
                    None
                };
            // Notes are appended as extra answers. For freeform questions, only submit when
            // the user explicitly committed the draft.
            let notes = if answer_state.answer_committed {
                answer_state.draft.text_with_pending().trim().to_string()
            } else {
                String::new()
            };
            let selected_label = selected_idx
                .and_then(|selected_idx| Self::option_label_for_index(question, selected_idx));
            let mut answer_list = selected_label.into_iter().collect::<Vec<_>>();
            if !notes.is_empty() {
                answer_list.push(format!("user_note: {notes}"));
            }
            answers.insert(
                question.id.clone(),
                RequestUserInputAnswer {
                    answers: answer_list,
                },
            );
        }
        self.app_event_tx
            .send(AppEvent::ChaosOp(Op::UserInputAnswer {
                id: self.request.turn_id.clone(),
                response: RequestUserInputResponse {
                    answers: answers.clone(),
                },
            }));
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::RequestUserInputResultCell {
                questions: self.request.questions.clone(),
                answers,
                interrupted: false,
            },
        )));
        if let Some(next) = self.queue.pop_front() {
            self.request = next;
            self.reset_for_request();
            self.ensure_focus_available();
            self.restore_current_draft();
        } else {
            self.done = true;
        }
    }

    /// Ensure the focus mode is valid for the current question.
    pub(super) fn ensure_focus_available(&mut self) {
        if self.question_count() == 0 {
            return;
        }
        if !self.has_options() {
            self.focus = Focus::Notes;
            if let Some(answer) = self.current_answer_mut() {
                answer.notes_visible = true;
            }
            return;
        }
        if matches!(self.focus, Focus::Notes) && !self.notes_ui_visible() {
            self.focus = Focus::Options;
            self.sync_composer_placeholder();
        }
    }

    /// Rebuild local answer state from the current request.
    pub(super) fn reset_for_request(&mut self) {
        self.answers = self
            .request
            .questions
            .iter()
            .map(|question| {
                let has_options = question
                    .options
                    .as_ref()
                    .is_some_and(|options| !options.is_empty());
                let mut options_state = ScrollState::new();
                if has_options {
                    options_state.selected_idx = Some(0);
                }
                super::AnswerState {
                    options_state,
                    draft: ComposerDraft::default(),
                    answer_committed: false,
                    notes_visible: !has_options,
                }
            })
            .collect();

        self.current_idx = 0;
        self.focus = Focus::Options;
        self.composer
            .set_text_content(String::new(), Vec::new(), Vec::new());
        self.confirm_unanswered = None;
        self.pending_submission_draft = None;
    }
}
