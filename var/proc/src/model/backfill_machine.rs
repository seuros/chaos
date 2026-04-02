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
    pub(crate) fn current_state(&self) -> &str {
        self.machine.current_state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backfill_happy_path() {
        let mut wf = BackfillWorkflow::new();
        assert_eq!(wf.current_state(), "Pending");

        assert!(wf.start());
        assert_eq!(wf.current_state(), "Running");

        assert!(wf.complete());
        assert_eq!(wf.current_state(), "Complete");
    }

    #[test]
    fn backfill_cannot_complete_from_pending() {
        let mut wf = BackfillWorkflow::new();
        assert!(!wf.complete());
        assert_eq!(wf.current_state(), "Pending");
    }

    #[test]
    fn backfill_cannot_start_from_running() {
        let mut wf = BackfillWorkflow::new();
        wf.start();
        assert!(!wf.start());
        assert_eq!(wf.current_state(), "Running");
    }

    #[test]
    fn from_status_roundtrip() {
        let pending = BackfillWorkflow::from_status(BackfillStatus::Pending);
        assert_eq!(pending.current_state(), "Pending");

        let running = BackfillWorkflow::from_status(BackfillStatus::Running);
        assert_eq!(running.current_state(), "Running");

        let complete = BackfillWorkflow::from_status(BackfillStatus::Complete);
        assert_eq!(complete.current_state(), "Complete");
    }
}
