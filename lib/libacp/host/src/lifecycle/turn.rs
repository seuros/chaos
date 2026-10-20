use state_machines::state_machine;

state_machine! {
    name: TurnLifecycle,
    dynamic: true,
    initial: Operation,
    states: [Operation, Prompting, Cancelling, Interrupted, JoiningCancellation, Finished],
    final_states: [Finished],
    events {
        prompt { transition: { from: Operation, to: Prompting } }
        cancel { transition: { from: Prompting, to: Cancelling } }
        cancelled {
            transition: { from: Cancelling, to: Interrupted }
            transition: { from: JoiningCancellation, to: Finished }
        }
        complete {
            transition: { from: Operation, to: Finished }
            transition: { from: Prompting, to: Finished }
            transition: { from: Cancelling, to: JoiningCancellation }
            transition: { from: Interrupted, to: Finished }
        }
        abandon {
            transition: { from: Operation, to: Finished }
            transition: { from: Prompting, to: Finished }
            transition: { from: Cancelling, to: Finished }
            transition: { from: Interrupted, to: Finished }
            transition: { from: JoiningCancellation, to: Finished }
        }
    }
}

#[derive(Debug)]
pub(crate) struct TurnState {
    machine: DynamicTurnLifecycle<()>,
}

impl TurnState {
    pub fn new(prompt: bool) -> Self {
        let mut machine = TurnLifecycle::new(()).into_dynamic();
        if prompt {
            assert!(
                machine.handle(TurnLifecycleEvent::Prompt).is_ok(),
                "a fresh operation can become a prompt"
            );
        }
        Self { machine }
    }

    pub fn is_live(&self) -> bool {
        matches!(
            self.machine.current_state(),
            TurnLifecycleState::Operation
                | TurnLifecycleState::Prompting
                | TurnLifecycleState::Cancelling
                | TurnLifecycleState::Interrupted
        )
    }

    pub fn request_cancel(&mut self) -> bool {
        self.machine.handle(TurnLifecycleEvent::Cancel).is_ok()
    }

    pub fn cancel_finished(&mut self) {
        let _ = self.machine.handle(TurnLifecycleEvent::Cancelled);
    }

    /// Stop admitting callbacks; report whether native cancellation must still join.
    pub fn complete(&mut self) -> bool {
        let _ = self.machine.handle(TurnLifecycleEvent::Complete);
        self.machine.current_state() == TurnLifecycleState::JoiningCancellation
    }

    /// Dropping a live prompt (or an unjoined cancellation) leaves work uncertain.
    pub fn abandon(&mut self) -> bool {
        let uncertain = !matches!(
            self.machine.current_state(),
            TurnLifecycleState::Operation | TurnLifecycleState::Finished
        );
        let _ = self.machine.handle(TurnLifecycleEvent::Abandon);
        uncertain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_and_completed_prompts_reject_cancel() {
        for prompt in [false, true] {
            let mut turn = TurnState::new(prompt);
            assert!(turn.is_live());
            if !prompt {
                assert!(!turn.request_cancel());
            }
            assert!(!turn.complete());
            assert!(!turn.is_live());
            assert!(!turn.request_cancel());
            assert!(!turn.complete());
            turn.cancel_finished();
            turn.abandon();
            assert!(!turn.is_live());
        }
    }

    #[test]
    fn native_cancel_must_join_even_when_prompt_completes_first() {
        let mut turn = TurnState::new(true);
        assert!(turn.request_cancel());
        assert!(!turn.request_cancel());
        assert!(turn.is_live());
        assert!(turn.complete());
        assert!(turn.complete());
        assert!(!turn.is_live());
        assert!(!turn.request_cancel());
        turn.cancel_finished();
        turn.cancel_finished();
        assert!(!turn.complete());
        assert!(!turn.is_live());
    }

    #[test]
    fn native_cancel_acknowledgement_does_not_complete_the_prompt() {
        let mut turn = TurnState::new(true);
        assert!(turn.request_cancel());
        turn.cancel_finished();
        assert!(turn.is_live());
        assert!(!turn.request_cancel());
        assert!(!turn.complete());
        assert!(!turn.is_live());
    }

    #[test]
    fn abandoning_an_inflight_turn_is_terminal_despite_late_callbacks() {
        let mut turn = TurnState::new(true);
        assert!(turn.request_cancel());
        assert!(turn.complete());
        turn.abandon();
        turn.cancel_finished();
        assert!(!turn.is_live());
        assert!(!turn.request_cancel());
        assert!(!turn.complete());
    }
}
