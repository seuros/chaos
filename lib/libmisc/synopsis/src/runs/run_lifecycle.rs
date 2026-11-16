use state_machines::state_machine;
use tokio::time::Instant;

use super::{RunError, RunOutcome, RunStatus};

#[derive(Clone, Copy, Debug)]
pub(super) struct Ended {
    pub outcome: RunOutcome,
    pub at: Instant,
}

state_machine! {
    name: RunLifecycle,
    dynamic: true,
    initial: Running,
    states: [Running, Cancelling, Terminal(Ended)],
    final_states: [Terminal],
    events {
        cancel {
            transition: { from: Running, to: Cancelling }
            transition: { from: Cancelling, internal: true }
        }
        terminate {
            payload: RunOutcome,
            transition: { from: Running, to: Terminal, data: ended }
            transition: { from: Cancelling, to: Terminal, data: ended }
        }
    }
}

impl<C, S> RunLifecycle<C, S> {
    #[expect(
        clippy::unused_self,
        reason = "state-machines data factories are methods on the source machine"
    )]
    fn ended(&self, outcome: &mut RunOutcome) -> Ended {
        Ended {
            outcome: *outcome,
            at: Instant::now(),
        }
    }
}

pub(super) struct Run {
    machine: DynamicRunLifecycle<()>,
}

impl Run {
    pub fn new() -> Self {
        Self {
            machine: RunLifecycle::new(()).into_dynamic(),
        }
    }

    pub fn ended(&self) -> Option<Ended> {
        self.machine.terminal_data().copied()
    }

    pub fn is_active(&self) -> bool {
        !self.machine.is_finished()
    }

    pub fn cancel(&mut self) -> bool {
        self.machine.handle(RunLifecycleEvent::Cancel).is_ok()
    }

    pub fn terminate(&mut self, outcome: RunOutcome) -> Result<(), RunError> {
        self.machine
            .handle(RunLifecycleEvent::Terminate(outcome))
            .map_err(|_| RunError::AlreadyTerminal)
    }

    pub fn status(&self) -> RunStatus {
        if let Some(ended) = self.ended() {
            return RunStatus::Terminal(ended.outcome);
        }
        match self.machine.current_state() {
            RunLifecycleState::Cancelling => RunStatus::Cancelling,
            RunLifecycleState::Running | RunLifecycleState::Terminal => RunStatus::Running,
        }
    }
}
