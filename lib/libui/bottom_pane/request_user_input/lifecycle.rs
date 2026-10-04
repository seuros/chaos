use super::{AnswerState, ComposerDraft, ScrollState};
pub(super) use focus::QuestionFocusState as Focus;
use state_machines::state_machine;

mod focus {
    use super::*;
    state_machine! {
        name: QuestionFocus,
        dynamic: true,
        initial: Options,
        states: [superstate Field { state Options, state Notes }],
        events {
            options { transition: { from: Field, to: Options } }
            notes { transition: { from: Field, to: Notes } }
        }
    }
}

pub(super) struct QuestionFocus {
    machine: focus::DynamicQuestionFocus<()>,
}

impl Default for QuestionFocus {
    fn default() -> Self {
        Self {
            machine: focus::DynamicQuestionFocus::new(()),
        }
    }
}

impl QuestionFocus {
    pub(super) fn state(&self) -> Focus {
        self.machine.current_state()
    }
    pub(super) fn options(&mut self) {
        assert!(
            self.machine
                .handle(focus::QuestionFocusEvent::Options)
                .is_ok()
        );
    }
    pub(super) fn notes(&mut self) {
        assert!(
            self.machine
                .handle(focus::QuestionFocusEvent::Notes)
                .is_ok()
        );
    }
}

#[derive(Default)]
pub(super) struct RequestData {
    pub answers: Vec<AnswerState>,
    pub current_idx: usize,
    pub focus: QuestionFocus,
    pub pending_submission_draft: Option<ComposerDraft>,
}

impl std::fmt::Debug for RequestData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestData")
            .field("answer_count", &self.answers.len())
            .field("current_idx", &self.current_idx)
            .field("focus", &self.focus.state())
            .finish_non_exhaustive()
    }
}

state_machine! {
    name: InputRequest,
    dynamic: true,
    initial: Editing,
    states: [superstate Active(RequestData) { state Editing, state Confirming(ScrollState), state Submitting }, Done],
    final_states: [Done],
    events {
        confirm { transition: { from: Editing, to: Confirming, data: confirmation } }
        back {
            transition: { from: Confirming, to: Editing }
            transition: { from: Editing, internal: true }
        }
        submit {
            transition: { from: Editing, to: Submitting }
            transition: { from: Confirming, to: Submitting }
        }
        next {
            transition: { from: Submitting, to: Editing }
            transition: { from: Editing, internal: true }
        }
        finish {
            transition: { from: Submitting, to: Done }
            transition: { from: Editing, to: Done }
            transition: { from: Confirming, to: Done }
        }
    }
}

impl<C, S> InputRequest<C, S> {
    fn confirmation(&self) -> ScrollState {
        let mut state = ScrollState::new();
        state.selected_idx = Some(0);
        state
    }
}

pub(super) struct RequestLifecycle {
    machine: DynamicInputRequest<()>,
}

impl Default for RequestLifecycle {
    fn default() -> Self {
        Self {
            machine: InputRequest::new(())
                .with_active_data(RequestData::default())
                .into_dynamic(),
        }
    }
}

impl RequestLifecycle {
    pub(super) fn data(&self) -> Option<&RequestData> {
        self.machine.active_data()
    }
    pub(super) fn data_mut(&mut self) -> Option<&mut RequestData> {
        self.machine.active_data_mut()
    }
    pub(super) fn confirmation(&self) -> Option<&ScrollState> {
        self.machine.confirming_data()
    }
    pub(super) fn confirmation_mut(&mut self) -> Option<&mut ScrollState> {
        self.machine.confirming_data_mut()
    }
    pub(super) fn reset(&mut self, answers: Vec<AnswerState>) -> bool {
        if !self.apply(InputRequestEvent::Next) {
            return false;
        }
        *self
            .data_mut()
            .unwrap_or_else(|| unreachable!("editing owns drafts")) = RequestData {
            answers,
            ..RequestData::default()
        };
        true
    }
    pub(super) fn done(&self) -> bool {
        self.machine.is_finished()
    }
    pub(super) fn confirming(&self) -> bool {
        self.machine.current_state() == InputRequestState::Confirming
    }
    pub(super) fn apply(&mut self, event: InputRequestEvent) -> bool {
        if self.done() && matches!(event, InputRequestEvent::Finish) {
            return true;
        }
        self.machine.handle(event).is_ok()
    }
}

#[cfg(test)]
mod tests;
