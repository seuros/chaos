use std::sync::Arc;

use state_machines::state_machine;

use crate::protocol;
use crate::state::{Turn, error, invalid};

state_machine! {
    name: SessionLifecycle,
    dynamic: true,
    initial: Opening,
    states: [Opening(Arc<Turn>), Idle, Busy(Arc<Turn>), InvalidBusy(Arc<Turn>), InvalidIdle],
    final_states: [InvalidIdle],
    events {
        begin {
            payload: Arc<Turn>,
            transition: { from: Idle, to: Busy, data: new_turn }
        }
        release {
            transition: { from: Opening, to: Idle }
            transition: { from: Busy, to: Idle }
            transition: { from: InvalidBusy, to: InvalidIdle }
        }
        invalidate {
            payload: Option<Arc<Turn>>,
            transition: { from: Opening, to: InvalidBusy, data: retain_turn }
            transition: { from: Busy, to: InvalidBusy, data: retain_turn }
            transition: { from: Idle, to: InvalidIdle }
        }
    }
}

impl<C, S> SessionLifecycle<C, S> {
    fn new_turn(&self, turn: &mut Arc<Turn>) -> Arc<Turn> {
        turn.clone()
    }

    fn retain_turn(&self, turn: &mut Option<Arc<Turn>>) -> Arc<Turn> {
        turn.take()
            .unwrap_or_else(|| unreachable!("invalidation retains the occupied session's turn"))
    }
}

pub(crate) struct Session {
    machine: DynamicSessionLifecycle<()>,
}

impl Session {
    pub fn opening(turn: Arc<Turn>) -> Self {
        Self {
            machine: SessionLifecycle::new(())
                .with_opening_data(turn)
                .into_dynamic(),
        }
    }

    pub fn active(&self) -> Option<&Arc<Turn>> {
        self.machine
            .opening_data()
            .or_else(|| self.machine.busy_data())
            .or_else(|| self.machine.invalid_busy_data())
    }

    pub fn is_invalid(&self) -> bool {
        matches!(
            self.machine.current_state(),
            SessionLifecycleState::InvalidBusy | SessionLifecycleState::InvalidIdle
        )
    }

    pub fn begin(&mut self, turn: Arc<Turn>) -> protocol::Result<()> {
        if self.is_invalid() {
            return Err(error("ACP session is invalid"));
        }
        self.machine
            .handle(SessionLifecycleEvent::Begin(turn))
            .map_err(|_| invalid("ACP session is busy"))
    }

    pub fn invalidate(&mut self) {
        let active = self.active().cloned();
        let _ = self
            .machine
            .handle(SessionLifecycleEvent::Invalidate(active));
    }

    pub fn release(&mut self, turn: &Arc<Turn>) {
        if self
            .active()
            .is_some_and(|active| Arc::ptr_eq(active, turn))
        {
            assert!(
                self.machine.handle(SessionLifecycleEvent::Release).is_ok(),
                "only an occupied session has an active turn"
            );
        }
    }

    pub fn check(&self, turn: &Arc<Turn>) -> protocol::Result<()> {
        if self.is_invalid()
            || !self
                .active()
                .is_some_and(|active| Arc::ptr_eq(active, turn))
            || !turn.is_live()
        {
            return Err(error("ACP prompt is no longer active"));
        }
        Ok(())
    }

    pub fn request_cancel(&self) -> Option<Arc<Turn>> {
        self.active().filter(|turn| turn.request_cancel()).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_and_replay_remain_exclusive_until_the_guard_releases() {
        let setup = Turn::new_operation(false);
        let prompt = Turn::new_operation(true);
        let mut session = Session::opening(setup.clone());
        assert!(session.check(&setup).is_ok());
        assert!(session.begin(prompt.clone()).is_err());
        assert!(session.request_cancel().is_none());
        session.release(&setup);
        assert!(session.check(&setup).is_err());
        session.begin(prompt.clone()).unwrap();
        // A delayed old guard must never clear a newer turn.
        session.release(&setup);
        assert!(session.check(&prompt).is_ok());
        assert!(session.begin(setup).is_err());
        assert!(session.request_cancel().is_some());
        assert!(session.request_cancel().is_none());
    }

    #[test]
    fn invalidation_retains_cleanup_identity_but_never_readmits_work() {
        for opening in [false, true] {
            let setup = Turn::new_operation(false);
            let turn = Turn::new_operation(true);
            let mut session = Session::opening(setup.clone());
            let active = if opening {
                setup
            } else {
                session.release(&setup);
                session.begin(turn.clone()).unwrap();
                turn
            };
            session.invalidate();
            session.invalidate();
            assert!(session.is_invalid());
            assert!(Arc::ptr_eq(session.active().unwrap(), &active));
            assert!(session.check(&active).is_err());
            session.release(&active);
            assert!(session.active().is_none());
            assert!(session.is_invalid());
            session.invalidate();
            assert!(session.begin(Turn::new_operation(true)).is_err());
        }
    }

    #[test]
    fn an_idle_session_can_be_invalidated_without_an_active_turn() {
        let turn = Turn::new_operation(false);
        let mut session = Session::opening(turn.clone());
        session.release(&turn);
        session.invalidate();
        assert!(session.is_invalid());
        assert!(session.active().is_none());
        assert!(session.begin(Turn::new_operation(true)).is_err());
    }
}
