use state_machines::runtime::Parallel;
use state_machines::state_machine;
use std::collections::HashMap;

pub use session::FrontendSessionState as SessionStatus;
pub use turn::FrontendTurnState as TurnStatus;

mod session {
    use super::*;
    state_machine! {
        name: FrontendSession,
        dynamic: true,
        initial: Booting,
        states: [Booting, Ready, Shutdown],
        final_states: [Shutdown],
        events {
            configure {
                transition: { from: Booting, to: Ready }
                transition: { from: Ready, internal: true }
            }
            shutdown {
                transition: { from: Booting, to: Shutdown }
                transition: { from: Ready, to: Shutdown }
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct PendingWork {
    pub streams: HashMap<String, usize>,
    pub calls: HashMap<String, usize>,
}

mod turn {
    use super::*;
    state_machine! {
        name: FrontendTurn,
        dynamic: true,
        initial: Idle,
        states: [superstate Turn(PendingWork) { state Idle, state InFlight }],
        events {
            submit { transition: { from: Turn, to: InFlight } }
            finish { transition: { from: Turn, to: Idle } }
        }
    }
}

pub(super) struct Lifecycle {
    regions: Parallel<session::DynamicFrontendSession<()>, turn::DynamicFrontendTurn<()>>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            regions: Parallel::new(
                session::DynamicFrontendSession::new(()),
                turn::FrontendTurn::new(())
                    .with_turn_data(PendingWork::default())
                    .into_dynamic(),
            ),
        }
    }
}

impl Clone for Lifecycle {
    fn clone(&self) -> Self {
        let mut turn = turn::DynamicFrontendTurn::new_init_state((), self.turn());
        assert!(turn.set_turn_data(self.pending().clone()).is_ok());
        Self {
            regions: Parallel::new(
                session::DynamicFrontendSession::new_init_state((), self.status()),
                turn,
            ),
        }
    }
}

impl std::fmt::Debug for Lifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lifecycle")
            .field("status", &self.status())
            .field("turn", &self.turn())
            .field("pending", self.pending())
            .finish()
    }
}

impl Lifecycle {
    pub(super) fn status(&self) -> SessionStatus {
        self.regions.left().current_state()
    }
    pub(super) fn turn(&self) -> TurnStatus {
        self.regions.right().current_state()
    }
    pub(super) fn pending(&self) -> &PendingWork {
        self.regions
            .right()
            .turn_data()
            .unwrap_or_else(|| unreachable!("turn owns bookkeeping"))
    }
    pub(super) fn pending_mut(&mut self) -> &mut PendingWork {
        self.regions
            .right_mut()
            .turn_data_mut()
            .unwrap_or_else(|| unreachable!("turn owns bookkeeping"))
    }
    pub(super) fn configure(&mut self) {
        let _ = self
            .regions
            .left_mut()
            .handle(session::FrontendSessionEvent::Configure);
    }
    pub(super) fn shutdown(&mut self) {
        if !self.regions.left().is_finished() {
            assert!(
                self.regions
                    .left_mut()
                    .handle(session::FrontendSessionEvent::Shutdown)
                    .is_ok()
            );
        }
        self.finish();
    }
    pub(super) fn submit(&mut self) {
        assert!(
            self.regions
                .right_mut()
                .handle(turn::FrontendTurnEvent::Submit)
                .is_ok()
        );
    }
    pub(super) fn finish(&mut self) {
        assert!(
            self.regions
                .right_mut()
                .handle(turn::FrontendTurnEvent::Finish)
                .is_ok()
        );
        *self.pending_mut() = PendingWork::default();
    }
}
