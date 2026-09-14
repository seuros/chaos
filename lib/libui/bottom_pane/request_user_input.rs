//! Request-user-input overlay state machine.
//!
//! Core behaviors:
//! - Each question can be answered by selecting one option and/or providing notes.
//! - Notes are stored per question and appended as extra answers.
//! - Typing while focused on options jumps into notes to keep freeform input fast.
//! - Enter advances to the next question; the last question submits all answers.
//! - Freeform-only questions submit an empty answer list when empty.
use std::collections::VecDeque;

mod layout;
mod render;

mod confirmation;
mod events;
mod navigation;

use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::ChatComposer;
use crate::bottom_pane::ChatComposerConfig;
use crate::bottom_pane::composer_draft::ComposerDraft;
use crate::bottom_pane::footer_tips::FooterTip;
use crate::bottom_pane::footer_tips::wrap_footer_tips;
use crate::bottom_pane::scroll_state::ScrollState;
use crate::bottom_pane::selection_popup_common::GenericDisplayRow;
use crate::bottom_pane::selection_popup_common::numbered_option_row;
use crate::bottom_pane::selection_popup_common::numbered_option_rows;
use crate::bottom_pane::selection_popup_common::option_index_for_digit as option_index_for_digit_shortcut;
use crate::bottom_pane::selection_popup_common::rows_required_height_with_default_selection;
use crate::render::renderable::Renderable;

use chaos_ipc::request_user_input::RequestUserInputEvent;

const NOTES_PLACEHOLDER: &str = "Add notes";
const ANSWER_PLACEHOLDER: &str = "Type your answer (optional)";
// Keep in sync with ChatComposer's minimum composer height.
const MIN_COMPOSER_HEIGHT: u16 = 3;
const SELECT_OPTION_PLACEHOLDER: &str = "Select an option to add notes";
pub(super) const TIP_SEPARATOR: &str = " | ";
pub(super) const DESIRED_SPACERS_BETWEEN_SECTIONS: u16 = 2;
const OTHER_OPTION_LABEL: &str = "None of the above";
const OTHER_OPTION_DESCRIPTION: &str = "Optionally, add details in notes (tab).";
const UNANSWERED_CONFIRM_TITLE: &str = "Submit with unanswered questions?";
const UNANSWERED_CONFIRM_GO_BACK: &str = "Go back";
const UNANSWERED_CONFIRM_GO_BACK_DESC: &str = "Return to the first unanswered question.";
const UNANSWERED_CONFIRM_SUBMIT: &str = "Proceed";
const UNANSWERED_CONFIRM_SUBMIT_DESC_SINGULAR: &str = "question";
const UNANSWERED_CONFIRM_SUBMIT_DESC_PLURAL: &str = "questions";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Options,
    Notes,
}

struct AnswerState {
    // Scrollable cursor state for option navigation/highlight.
    options_state: ScrollState,
    // Per-question notes draft.
    draft: ComposerDraft,
    // Whether the answer for this question has been explicitly submitted.
    answer_committed: bool,
    // Whether the notes UI has been explicitly opened for this question.
    notes_visible: bool,
}

pub struct RequestUserInputOverlay {
    app_event_tx: AppEventSender,
    request: RequestUserInputEvent,
    // Queue of incoming requests to process after the current one.
    queue: VecDeque<RequestUserInputEvent>,
    // Reuse the shared chat composer so notes/freeform answers match the
    // primary input styling and behavior.
    composer: ChatComposer,
    // One entry per question: selection state plus a stored notes draft.
    answers: Vec<AnswerState>,
    current_idx: usize,
    focus: Focus,
    done: bool,
    pending_submission_draft: Option<ComposerDraft>,
    confirm_unanswered: Option<ScrollState>,
}

impl RequestUserInputOverlay {
    pub fn new(
        request: RequestUserInputEvent,
        app_event_tx: AppEventSender,
        has_input_focus: bool,
        enhanced_keys_supported: bool,
        disable_paste_burst: bool,
    ) -> Self {
        // Use the same composer widget, but disable popups/slash-commands and
        // image-path attachment so it behaves like a focused notes field.
        let mut composer = ChatComposer::new_with_config(
            has_input_focus,
            app_event_tx.clone(),
            enhanced_keys_supported,
            ANSWER_PLACEHOLDER.to_string(),
            disable_paste_burst,
            ChatComposerConfig::plain_text(),
        );
        // The overlay renders its own footer hints, so keep the composer footer empty.
        composer.set_footer_hint_override(Some(Vec::new()));
        let mut overlay = Self {
            app_event_tx,
            request,
            queue: VecDeque::new(),
            composer,
            answers: Vec::new(),
            current_idx: 0,
            focus: Focus::Options,
            done: false,
            pending_submission_draft: None,
            confirm_unanswered: None,
        };
        overlay.reset_for_request();
        overlay.ensure_focus_available();
        overlay.restore_current_draft();
        overlay
    }

    fn current_index(&self) -> usize {
        self.current_idx
    }

    fn current_question(&self) -> Option<&chaos_ipc::request_user_input::RequestUserInputQuestion> {
        self.request.questions.get(self.current_index())
    }

    fn current_answer_mut(&mut self) -> Option<&mut AnswerState> {
        let idx = self.current_index();
        self.answers.get_mut(idx)
    }

    fn current_answer(&self) -> Option<&AnswerState> {
        let idx = self.current_index();
        self.answers.get(idx)
    }

    fn question_count(&self) -> usize {
        self.request.questions.len()
    }

    fn has_options(&self) -> bool {
        self.current_question()
            .and_then(|question| question.options.as_ref())
            .is_some_and(|options| !options.is_empty())
    }

    fn options_len(&self) -> usize {
        self.current_question()
            .map(Self::options_len_for_question)
            .unwrap_or(0)
    }

    fn option_index_for_digit(&self, ch: char) -> Option<usize> {
        if !self.has_options() {
            return None;
        }
        option_index_for_digit_shortcut(ch, self.options_len())
    }

    fn selected_option_index(&self) -> Option<usize> {
        if !self.has_options() {
            return None;
        }
        self.current_answer()
            .and_then(|answer| answer.options_state.selected_idx)
    }

    fn notes_has_content(&self, idx: usize) -> bool {
        if idx == self.current_index() {
            !self.composer.current_text_with_pending().trim().is_empty()
        } else {
            !self.answers[idx].draft.text.trim().is_empty()
        }
    }

    pub(super) fn notes_ui_visible(&self) -> bool {
        if !self.has_options() {
            return true;
        }
        let idx = self.current_index();
        self.current_answer()
            .is_some_and(|answer| answer.notes_visible || self.notes_has_content(idx))
    }

    pub(super) fn wrapped_question_lines(&self, width: u16) -> Vec<String> {
        self.current_question()
            .map(|q| {
                textwrap::wrap(&q.question, width.max(1) as usize)
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    fn focus_is_notes(&self) -> bool {
        matches!(self.focus, Focus::Notes)
    }

    fn confirm_unanswered_active(&self) -> bool {
        self.confirm_unanswered.is_some()
    }

    pub(super) fn option_rows(&self) -> Vec<GenericDisplayRow> {
        self.current_question()
            .and_then(|question| question.options.as_ref().map(|options| (question, options)))
            .map(|(question, options)| {
                let selected_idx = self
                    .current_answer()
                    .and_then(|answer| answer.options_state.selected_idx);
                let mut rows = numbered_option_rows(
                    options
                        .iter()
                        .map(|opt| (opt.label.clone(), Some(opt.description.clone()))),
                    selected_idx,
                );

                if Self::other_option_enabled_for_question(question) {
                    let idx = options.len();
                    rows.push(numbered_option_row(
                        idx,
                        OTHER_OPTION_LABEL,
                        Some(OTHER_OPTION_DESCRIPTION),
                        selected_idx,
                    ));
                }

                rows
            })
            .unwrap_or_default()
    }

    pub(super) fn options_required_height(&self, width: u16) -> u16 {
        if !self.has_options() {
            return 0;
        }

        let rows = self.option_rows();
        let state = self
            .current_answer()
            .map(|answer| answer.options_state)
            .unwrap_or_default();
        rows_required_height_with_default_selection(&rows, state, width, 1)
    }

    pub(super) fn options_preferred_height(&self, width: u16) -> u16 {
        if !self.has_options() {
            return 0;
        }

        let rows = self.option_rows();
        let state = self
            .current_answer()
            .map(|answer| answer.options_state)
            .unwrap_or_default();
        rows_required_height_with_default_selection(&rows, state, width, 1)
    }

    fn capture_composer_draft(&self) -> ComposerDraft {
        ComposerDraft {
            text: self.composer.current_text(),
            text_elements: self.composer.text_elements(),
            local_image_paths: self
                .composer
                .local_images()
                .into_iter()
                .map(|img| img.path)
                .collect(),
            pending_pastes: self.composer.pending_pastes(),
        }
    }

    fn save_current_draft(&mut self) {
        let draft = self.capture_composer_draft();
        let notes_empty = draft.text.trim().is_empty();
        if let Some(answer) = self.current_answer_mut() {
            if answer.answer_committed && answer.draft != draft {
                answer.answer_committed = false;
            }
            answer.draft = draft;
            if !notes_empty {
                answer.notes_visible = true;
            }
        }
    }

    fn restore_current_draft(&mut self) {
        self.composer
            .set_placeholder_text(self.notes_placeholder().to_string());
        self.composer.set_footer_hint_override(Some(Vec::new()));
        let Some(answer) = self.current_answer() else {
            self.composer
                .set_text_content(String::new(), Vec::new(), Vec::new());
            self.composer.move_cursor_to_end();
            return;
        };
        let draft = answer.draft.clone();
        self.composer
            .set_text_content(draft.text, draft.text_elements, draft.local_image_paths);
        self.composer.set_pending_pastes(draft.pending_pastes);
        self.composer.move_cursor_to_end();
    }

    fn notes_placeholder(&self) -> &'static str {
        if self.has_options() && self.selected_option_index().is_none() {
            SELECT_OPTION_PLACEHOLDER
        } else if self.has_options() {
            NOTES_PLACEHOLDER
        } else {
            ANSWER_PLACEHOLDER
        }
    }

    fn sync_composer_placeholder(&mut self) {
        self.composer
            .set_placeholder_text(self.notes_placeholder().to_string());
    }

    fn clear_notes_draft(&mut self) {
        if let Some(answer) = self.current_answer_mut() {
            answer.draft = ComposerDraft::default();
            answer.answer_committed = false;
            answer.notes_visible = true;
        }
        self.pending_submission_draft = None;
        self.composer
            .set_text_content(String::new(), Vec::new(), Vec::new());
        self.composer.move_cursor_to_end();
        self.sync_composer_placeholder();
    }

    fn footer_tips(&self) -> Vec<FooterTip> {
        let mut tips = Vec::new();
        let notes_visible = self.notes_ui_visible();
        if self.has_options() {
            if self.selected_option_index().is_some() && !notes_visible {
                tips.push(FooterTip::highlighted("tab to add notes"));
            }
            if self.selected_option_index().is_some() && notes_visible {
                tips.push(FooterTip::new("tab or esc to clear notes"));
            }
        }

        let question_count = self.question_count();
        let is_last_question = self.current_index().saturating_add(1) >= question_count;
        let enter_tip = if question_count == 1 {
            FooterTip::highlighted("enter to submit answer")
        } else if is_last_question {
            FooterTip::highlighted("enter to submit all")
        } else {
            FooterTip::new("enter to submit answer")
        };
        tips.push(enter_tip);
        if question_count > 1 {
            if self.has_options() && !self.focus_is_notes() {
                tips.push(FooterTip::new("←/→ to navigate questions"));
            } else if !self.has_options() {
                tips.push(FooterTip::new("ctrl + p / ctrl + n change question"));
            }
        }
        if !(self.has_options() && notes_visible) {
            tips.push(FooterTip::new("esc to interrupt"));
        }
        tips
    }

    pub(super) fn footer_tip_lines(&self, width: u16) -> Vec<Vec<FooterTip>> {
        wrap_footer_tips(width, TIP_SEPARATOR, self.footer_tips())
    }

    pub(super) fn footer_tip_lines_with_prefix(
        &self,
        width: u16,
        prefix: Option<FooterTip>,
    ) -> Vec<Vec<FooterTip>> {
        let mut tips = Vec::new();
        if let Some(prefix) = prefix {
            tips.push(prefix);
        }
        tips.extend(self.footer_tips());
        wrap_footer_tips(width, TIP_SEPARATOR, tips)
    }

    pub(super) fn footer_required_height(&self, width: u16) -> u16 {
        self.footer_tip_lines(width).len() as u16
    }

    fn options_len_for_question(
        question: &chaos_ipc::request_user_input::RequestUserInputQuestion,
    ) -> usize {
        let options_len = question
            .options
            .as_ref()
            .map(std::vec::Vec::len)
            .unwrap_or(0);
        if Self::other_option_enabled_for_question(question) {
            options_len + 1
        } else {
            options_len
        }
    }

    fn other_option_enabled_for_question(
        question: &chaos_ipc::request_user_input::RequestUserInputQuestion,
    ) -> bool {
        question.is_other
            && question
                .options
                .as_ref()
                .is_some_and(|options| !options.is_empty())
    }

    fn option_label_for_index(
        question: &chaos_ipc::request_user_input::RequestUserInputQuestion,
        idx: usize,
    ) -> Option<String> {
        let options = question.options.as_ref()?;
        if idx < options.len() {
            return options.get(idx).map(|opt| opt.label.clone());
        }
        if idx == options.len() && Self::other_option_enabled_for_question(question) {
            return Some(OTHER_OPTION_LABEL.to_string());
        }
        None
    }

    /// Compute the preferred notes input height for the current question.
    fn notes_input_height(&self, width: u16) -> u16 {
        let min_height = MIN_COMPOSER_HEIGHT;
        self.composer
            .desired_height(width.max(1))
            .clamp(min_height, min_height.saturating_add(5))
    }
}

#[cfg(test)]
pub(crate) mod tests;
