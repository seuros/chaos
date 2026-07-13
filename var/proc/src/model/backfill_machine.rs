use crate::BackfillStatus;
use state_machines::state_machine;

// Backfill lifecycle: Pending → Running → Complete
state_machine! {
    name: BackfillLifecycle,
    dynamic: true,
    initial: Pending,
    states: [Pending, Running, Complete],
    events {
        start {
            transition: { from: Pending, to: Running }
        }
        complete {
            transition: { from: Running, to: Complete }
        }
    }
}

#[derive(Debug)]
pub(crate) struct BackfillWorkflow {
    machine: DynamicBackfillLifecycle<()>,
}

impl BackfillWorkflow {
    pub(crate) fn new() -> Self {
        Self {
            machine: DynamicBackfillLifecycle::new(()),
        }
    }

    /// Reconstruct the workflow at a known persisted state.
    pub(crate) fn from_status(status: BackfillStatus) -> Self {
        let mut wf = Self::new();
        match status {
            BackfillStatus::Pending => {}
            BackfillStatus::Running => {
                wf.start();
            }
            BackfillStatus::Complete => {
                wf.start();
                wf.complete();
            }
        }
        wf
    }

    pub(crate) fn start(&mut self) -> bool {
        self.machine.handle(BackfillLifecycleEvent::Start).is_ok()
    }

    pub(crate) fn complete(&mut self) -> bool {
        self.machine
            .handle(BackfillLifecycleEvent::Complete)
            .is_ok()
    }

    #[cfg(test)]
    pub(crate) fn current_state(&self) -> BackfillLifecycleState {
        self.machine.current_state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backfill_workflow_transitions_and_persisted_status_replay() {
        let mut wf = BackfillWorkflow::new();
        assert_eq!(wf.current_state(), BackfillLifecycleState::Pending);

        assert!(wf.start());
        assert_eq!(wf.current_state(), BackfillLifecycleState::Running);

        assert!(wf.complete());
        assert_eq!(wf.current_state(), BackfillLifecycleState::Complete);

        let mut wf = BackfillWorkflow::new();
        assert!(!wf.complete());
        assert_eq!(wf.current_state(), BackfillLifecycleState::Pending);

        let mut wf = BackfillWorkflow::new();
        wf.start();
        assert!(!wf.start());
        assert_eq!(wf.current_state(), BackfillLifecycleState::Running);

        for (status, expected) in [
            (BackfillStatus::Pending, BackfillLifecycleState::Pending),
            (BackfillStatus::Running, BackfillLifecycleState::Running),
            (BackfillStatus::Complete, BackfillLifecycleState::Complete),
        ] {
            let wf = BackfillWorkflow::from_status(status);
            assert_eq!(wf.current_state(), expected);
        }
    }
}
