//! The transition graph is separate from evidence/authorization predicates.
use super::Status;
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

impl Status {
    pub fn accepts(self, event: GoalLifecycleEvent) -> bool {
        let state = match self {
            Self::Active => GoalLifecycleState::Active,
            Self::Checking => GoalLifecycleState::Checking,
            Self::Complete => GoalLifecycleState::Complete,
            Self::Paused => GoalLifecycleState::Paused,
        };
        DynamicGoalLifecycle::new_init_state((), state)
            .handle(event)
            .is_ok()
    }
}
