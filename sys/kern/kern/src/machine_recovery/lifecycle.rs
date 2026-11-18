use state_machines::state_machine;

#[derive(Debug)]
struct WaitRequest {
    id: String,
    turn: String,
}

#[derive(Debug)]
struct ParkedWait {
    id: String,
}

mod wait {
    use super::*;

    state_machine! {
        name: RecoveryWait,
        dynamic: true,
        initial: Idle,
        states: [Idle, Requested(WaitRequest), Parked(ParkedWait)],
        events {
            request {
                payload: Option<WaitRequest>,
                transition: { from: Idle, to: Requested, data: own_request }
            }
            park {
                payload: Option<ParkedWait>,
                transition: { from: Requested, to: Parked, data: own_wait }
            }
            cancel {
                transition: { from: Idle, internal: true }
                transition: { from: Requested, to: Idle }
                transition: { from: Parked, to: Idle }
            }
        }
    }

    impl<C, S> RecoveryWait<C, S> {
        fn own_request(&self, request: &mut Option<WaitRequest>) -> WaitRequest {
            request
                .take()
                .unwrap_or_else(|| unreachable!("request owns wait registration"))
        }

        fn own_wait(&self, wait: &mut Option<ParkedWait>) -> ParkedWait {
            wait.take()
                .unwrap_or_else(|| unreachable!("park owns wait registration"))
        }
    }
}

pub(super) struct Wait {
    machine: wait::DynamicRecoveryWait<()>,
}

impl Default for Wait {
    fn default() -> Self {
        Self {
            machine: wait::DynamicRecoveryWait::new(()),
        }
    }
}

impl Wait {
    pub(super) fn id(&self) -> Option<&str> {
        self.machine
            .requested_data()
            .map(|data| data.id.as_str())
            .or_else(|| self.machine.parked_data().map(|data| data.id.as_str()))
    }

    pub(super) fn requested_turn(&self) -> Option<&str> {
        self.machine.requested_data().map(|data| data.turn.as_str())
    }

    pub(super) fn parked(&self) -> bool {
        self.machine.current_state() == wait::RecoveryWaitState::Parked
    }

    pub(super) fn request(&mut self, id: String, turn: String) {
        assert!(
            self.machine
                .handle(wait::RecoveryWaitEvent::Request(Some(WaitRequest {
                    id,
                    turn
                })))
                .is_ok()
        );
    }

    pub(super) fn park(&mut self, turn: &str) -> Option<String> {
        if self.requested_turn() != Some(turn) {
            return None;
        }
        let id = self
            .id()
            .unwrap_or_else(|| unreachable!("requested wait owns id"))
            .to_owned();
        assert!(
            self.machine
                .handle(wait::RecoveryWaitEvent::Park(Some(ParkedWait {
                    id: id.clone()
                })))
                .is_ok()
        );
        Some(id)
    }

    pub(super) fn cancel(&mut self) -> Option<String> {
        let id = self.id().map(str::to_owned);
        assert!(self.machine.handle(wait::RecoveryWaitEvent::Cancel).is_ok());
        id
    }
}
