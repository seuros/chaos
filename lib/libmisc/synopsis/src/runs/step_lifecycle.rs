use state_machines::state_machine;

use super::{RunError, StepOutcome, StepStatus};

state_machine! {
    name: StepLifecycle,
    dynamic: true,
    initial: Running,
    states: [Running, WaitingForInput, Terminal(StepOutcome)],
    final_states: [Terminal],
    events {
        wait {
            transition: { from: Running, to: WaitingForInput }
            transition: { from: WaitingForInput, internal: true }
        }
        finish {
            payload: StepOutcome,
            transition: { from: Running, to: Terminal, data: outcome }
            transition: { from: WaitingForInput, to: Terminal, data: outcome }
        }
    }
}

impl<C, S> StepLifecycle<C, S> {
    #[expect(
        clippy::unused_self,
        reason = "state-machines data factories are methods on the source machine"
    )]
    fn outcome(&self, outcome: &mut StepOutcome) -> StepOutcome {
        *outcome
    }
}

fn restore(status: StepStatus) -> DynamicStepLifecycle<()> {
    let state = match status {
        StepStatus::Running => StepLifecycleState::Running,
        StepStatus::WaitingForInput => StepLifecycleState::WaitingForInput,
        StepStatus::Terminal(_) => StepLifecycleState::Terminal,
    };
    DynamicStepLifecycle::new_init_state((), state)
}

impl StepStatus {
    pub(super) fn is_terminal(self) -> bool {
        restore(self).is_finished()
    }

    pub(super) fn wait(self) -> Result<Self, RunError> {
        self.after(StepLifecycleEvent::Wait)
    }

    pub(super) fn finish(self, outcome: StepOutcome) -> Result<Self, RunError> {
        self.after(StepLifecycleEvent::Finish(outcome))
    }

    fn after(self, event: StepLifecycleEvent) -> Result<Self, RunError> {
        let mut machine = restore(self);
        machine
            .handle(event)
            .map_err(|_| RunError::InvalidTransition)?;
        match machine.current_state() {
            StepLifecycleState::Running => Ok(Self::Running),
            StepLifecycleState::WaitingForInput => Ok(Self::WaitingForInput),
            StepLifecycleState::Terminal => machine
                .terminal_data()
                .copied()
                .map(Self::Terminal)
                .ok_or(RunError::InvalidTransition),
        }
    }
}
