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
    control: Option<Arc<Turn>>,
}

impl Session {
    pub fn opening(turn: Arc<Turn>) -> Self {
        Self {
            machine: SessionLifecycle::new(())
                .with_opening_data(turn)
                .into_dynamic(),
            control: None,
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

    pub fn begin_control(&mut self, turn: Arc<Turn>) -> protocol::Result<()> {
        if self.is_invalid() {
            return Err(error("ACP session is invalid"));
        }
        if self.machine.current_state() == SessionLifecycleState::Opening {
            return Err(invalid("ACP session is opening"));
        }
        if self.control.is_some() {
            return Err(invalid("ACP session configuration is busy"));
        }
        self.control = Some(turn);
        Ok(())
    }

    pub fn invalidate(&mut self) {
        let active = self.active().cloned();
        let _ = self
            .machine
            .handle(SessionLifecycleEvent::Invalidate(active));
    }

    pub fn release(&mut self, turn: &Arc<Turn>) {
        if self
            .control
            .as_ref()
            .is_some_and(|control| Arc::ptr_eq(control, turn))
        {
            self.control = None;
            return;
        }
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
            || !(self
                .active()
                .is_some_and(|active| Arc::ptr_eq(active, turn))
                || self
                    .control
                    .as_ref()
                    .is_some_and(|control| Arc::ptr_eq(control, turn)))
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
mod tests;
