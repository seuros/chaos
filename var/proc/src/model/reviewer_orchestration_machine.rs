use crate::ReviewAttemptState;
use state_machines::state_machine;

state_machine! {
    name: ReviewAttemptLifecycle,
    dynamic: true,
    initial: Selection,
    states: [
        Selection,
        Spawn,
        ModelExecution,
        OutputParse,
        SubmissionUnknown,
        Acknowledged,
        Cancelled,
        TerminalFailure
    ],
    events {
        select {
            transition: { from: Selection, to: Spawn }
        }
        spawned {
            transition: { from: Spawn, to: ModelExecution }
        }
        output_received {
            transition: { from: ModelExecution, to: OutputParse }
        }
        output_parsed {
            transition: { from: OutputParse, to: SubmissionUnknown }
        }
        acknowledge {
            transition: { from: SubmissionUnknown, to: Acknowledged }
        }
        cancel {
            transition: { from: Selection, to: Cancelled }
            transition: { from: Spawn, to: Cancelled }
            transition: { from: ModelExecution, to: Cancelled }
            transition: { from: OutputParse, to: Cancelled }
        }
        fail {
            transition: { from: Selection, to: TerminalFailure }
            transition: { from: Spawn, to: TerminalFailure }
            transition: { from: ModelExecution, to: TerminalFailure }
            transition: { from: OutputParse, to: TerminalFailure }
            transition: { from: SubmissionUnknown, to: TerminalFailure }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ReviewAttemptWorkflow {
    machine: DynamicReviewAttemptLifecycle<()>,
}

impl ReviewAttemptWorkflow {
    /// Restore persisted state without replaying transitions or callbacks.
    pub(crate) fn from_state(state: ReviewAttemptState) -> Self {
        let state = match state {
            ReviewAttemptState::Selection => ReviewAttemptLifecycleState::Selection,
            ReviewAttemptState::Spawn => ReviewAttemptLifecycleState::Spawn,
            ReviewAttemptState::ModelExecution => ReviewAttemptLifecycleState::ModelExecution,
            ReviewAttemptState::OutputParse => ReviewAttemptLifecycleState::OutputParse,
            ReviewAttemptState::SubmissionUnknown => ReviewAttemptLifecycleState::SubmissionUnknown,
            ReviewAttemptState::Acknowledged => ReviewAttemptLifecycleState::Acknowledged,
            ReviewAttemptState::Cancelled => ReviewAttemptLifecycleState::Cancelled,
            ReviewAttemptState::TerminalFailure => ReviewAttemptLifecycleState::TerminalFailure,
        };
        Self {
            machine: DynamicReviewAttemptLifecycle::new_init_state((), state),
        }
    }

    pub(crate) fn permits(&mut self, target: ReviewAttemptState) -> bool {
        let event = match target {
            ReviewAttemptState::Spawn => ReviewAttemptLifecycleEvent::Select,
            ReviewAttemptState::ModelExecution => ReviewAttemptLifecycleEvent::Spawned,
            ReviewAttemptState::OutputParse => ReviewAttemptLifecycleEvent::OutputReceived,
            ReviewAttemptState::SubmissionUnknown => ReviewAttemptLifecycleEvent::OutputParsed,
            ReviewAttemptState::Acknowledged => ReviewAttemptLifecycleEvent::Acknowledge,
            ReviewAttemptState::Cancelled => ReviewAttemptLifecycleEvent::Cancel,
            ReviewAttemptState::TerminalFailure => ReviewAttemptLifecycleEvent::Fail,
            ReviewAttemptState::Selection => return false,
        };
        self.handle(event)
    }

    fn handle(&mut self, event: ReviewAttemptLifecycleEvent) -> bool {
        self.machine.handle(event).is_ok()
    }
}

#[cfg(test)]
mod tests;
