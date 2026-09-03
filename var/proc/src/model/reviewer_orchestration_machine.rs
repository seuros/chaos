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
    pub(crate) fn from_state(state: ReviewAttemptState) -> Self {
        let mut workflow = Self {
            machine: DynamicReviewAttemptLifecycle::new(()),
        };
        match state {
            ReviewAttemptState::Selection => {}
            ReviewAttemptState::Spawn => {
                workflow.handle(ReviewAttemptLifecycleEvent::Select);
            }
            ReviewAttemptState::ModelExecution => {
                workflow.handle(ReviewAttemptLifecycleEvent::Select);
                workflow.handle(ReviewAttemptLifecycleEvent::Spawned);
            }
            ReviewAttemptState::OutputParse => {
                workflow.handle(ReviewAttemptLifecycleEvent::Select);
                workflow.handle(ReviewAttemptLifecycleEvent::Spawned);
                workflow.handle(ReviewAttemptLifecycleEvent::OutputReceived);
            }
            ReviewAttemptState::SubmissionUnknown => {
                workflow.handle(ReviewAttemptLifecycleEvent::Select);
                workflow.handle(ReviewAttemptLifecycleEvent::Spawned);
                workflow.handle(ReviewAttemptLifecycleEvent::OutputReceived);
                workflow.handle(ReviewAttemptLifecycleEvent::OutputParsed);
            }
            ReviewAttemptState::Acknowledged => {
                workflow.handle(ReviewAttemptLifecycleEvent::Select);
                workflow.handle(ReviewAttemptLifecycleEvent::Spawned);
                workflow.handle(ReviewAttemptLifecycleEvent::OutputReceived);
                workflow.handle(ReviewAttemptLifecycleEvent::OutputParsed);
                workflow.handle(ReviewAttemptLifecycleEvent::Acknowledge);
            }
            ReviewAttemptState::Cancelled => {
                workflow.handle(ReviewAttemptLifecycleEvent::Cancel);
            }
            ReviewAttemptState::TerminalFailure => {
                workflow.handle(ReviewAttemptLifecycleEvent::Fail);
            }
        }
        workflow
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
mod tests {
    use super::*;

    #[test]
    fn persisted_states_reconstruct_and_only_allow_declared_transitions() {
        let cases = [
            (ReviewAttemptState::Selection, ReviewAttemptState::Spawn),
            (
                ReviewAttemptState::Spawn,
                ReviewAttemptState::ModelExecution,
            ),
            (
                ReviewAttemptState::ModelExecution,
                ReviewAttemptState::OutputParse,
            ),
            (
                ReviewAttemptState::OutputParse,
                ReviewAttemptState::SubmissionUnknown,
            ),
            (
                ReviewAttemptState::SubmissionUnknown,
                ReviewAttemptState::Acknowledged,
            ),
        ];
        for (from, to) in cases {
            assert!(ReviewAttemptWorkflow::from_state(from).permits(to));
        }

        assert!(
            !ReviewAttemptWorkflow::from_state(ReviewAttemptState::Acknowledged)
                .permits(ReviewAttemptState::SubmissionUnknown)
        );
        assert!(
            ReviewAttemptWorkflow::from_state(ReviewAttemptState::SubmissionUnknown)
                .permits(ReviewAttemptState::TerminalFailure)
        );
        for state in [
            ReviewAttemptState::Selection,
            ReviewAttemptState::Spawn,
            ReviewAttemptState::ModelExecution,
            ReviewAttemptState::OutputParse,
        ] {
            assert!(
                ReviewAttemptWorkflow::from_state(state).permits(ReviewAttemptState::Cancelled)
            );
        }
        assert!(
            !ReviewAttemptWorkflow::from_state(ReviewAttemptState::SubmissionUnknown)
                .permits(ReviewAttemptState::Cancelled)
        );
    }
}
