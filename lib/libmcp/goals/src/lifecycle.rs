//! The transition graph is separate from evidence/authorization predicates.
use crate::Status;
use state_machines::state_machine;

state_machine! {
    name: GoalLifecycle,
    dynamic: true,
    initial: Active,
    states: [Active, Checking, Complete, Paused],
    events {
        pause {
            payload: bool, guards: [approved],
            transition: { from: Active, to: Paused }
            transition: { from: Checking, to: Paused }
            transition: { from: Complete, to: Paused }
        }
        reopen {
            payload: bool, guards: [approved],
            transition: { from: Complete, to: Active }
        }
        check {
            payload: bool, guards: [approved],
            transition: { from: Active, to: Checking }
        }
        pass {
            payload: bool, guards: [approved],
            transition: { from: Checking, to: Complete }
        }
        retry {
            payload: bool, guards: [approved],
            transition: { from: Checking, to: Active }
        }
        stop {
            payload: bool, guards: [approved],
            transition: { from: Checking, to: Paused }
        }
    }
}

impl<C, S> GoalLifecycle<C, S> {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "state-machines guards receive event payloads by reference"
    )]
    fn approved(&self, _ctx: &C, approved: &bool) -> bool {
        *approved
    }
}

impl From<Status> for GoalLifecycleState {
    fn from(status: Status) -> Self {
        match status {
            Status::Active => Self::Active,
            Status::Checking => Self::Checking,
            Status::Complete => Self::Complete,
            Status::Paused => Self::Paused,
        }
    }
}

impl From<GoalLifecycleState> for Status {
    fn from(state: GoalLifecycleState) -> Self {
        match state {
            GoalLifecycleState::Active => Self::Active,
            GoalLifecycleState::Checking => Self::Checking,
            GoalLifecycleState::Complete => Self::Complete,
            GoalLifecycleState::Paused => Self::Paused,
        }
    }
}

impl Status {
    pub fn next(self, event: GoalLifecycleEvent) -> Option<Self> {
        let mut machine = DynamicGoalLifecycle::new_init_state((), self.into());
        machine.handle(event).ok()?;
        Some(machine.current_state().into())
    }

    pub fn accepts(self, event: GoalLifecycleEvent) -> bool {
        self.next(event).is_some()
    }
}
