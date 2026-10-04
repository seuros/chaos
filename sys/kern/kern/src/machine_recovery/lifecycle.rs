use super::{MachineWarning, Phase};
use state_machines::state_machine;
use std::time::SystemTime;
use tokio::time::Instant;

#[derive(Debug, Default)]
pub(super) struct ObservationData {
    pub outstanding: Vec<MachineWarning>,
    pub last_sample: Option<(Instant, SystemTime)>,
    pub blocked_reason: Option<String>,
    pub recovered_episode: bool,
}

#[derive(Debug)]
pub(super) struct Stability {
    since: Instant,
}

state_machine! {
    name: RecoveryObservation,
    dynamic: true,
    initial: Healthy,
    states: [
        superstate Observed(ObservationData) {
            state Healthy,
            superstate Episode {
                state Warning, state Stabilizing(Stability), state Recovered(Stability),
                state Unavailable, state Blocked,
            }
        }
    ],
    events {
        warn { transition: { from: Observed, to: Warning } }
        healthy { transition: { from: Healthy, to: Healthy } }
        unavailable { transition: { from: Episode, to: Unavailable } }
        blocked { transition: { from: Episode, to: Blocked } }
        stable {
            payload: Instant,
            transition: { from: Episode, to: Recovered, data: stability }
        }
        headroom {
            payload: Instant,
            transition: { from: Episode, to: Stabilizing, data: stability }
        }
    }
}

impl<C, S> RecoveryObservation<C, S> {
    fn stability(&self, since: &mut Instant) -> Stability {
        Stability { since: *since }
    }
}

pub(super) struct Observation {
    machine: DynamicRecoveryObservation<()>,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            machine: RecoveryObservation::new(())
                .with_observed_data(ObservationData::default())
                .into_dynamic(),
        }
    }
}

impl Observation {
    pub(super) fn data(&self) -> &ObservationData {
        self.machine
            .observed_data()
            .unwrap_or_else(|| unreachable!("observation owns data"))
    }

    pub(super) fn data_mut(&mut self) -> &mut ObservationData {
        self.machine
            .observed_data_mut()
            .unwrap_or_else(|| unreachable!("observation owns data"))
    }

    pub(super) fn healthy_since(&self) -> Option<Instant> {
        self.machine
            .stabilizing_data()
            .or_else(|| self.machine.recovered_data())
            .map(|data| data.since)
    }

    pub(super) fn reset_stability(&mut self) {
        if self.healthy_since().is_some() {
            self.observe(RecoveryObservationEvent::Warn);
        }
    }

    pub(super) fn observe(&mut self, event: RecoveryObservationEvent) {
        assert!(
            self.machine.handle(event).is_ok(),
            "observation belongs to the current recovery episode"
        );
    }

    pub(super) fn phase(&self) -> Phase {
        match self.machine.current_state() {
            RecoveryObservationState::Healthy => Phase::Healthy,
            RecoveryObservationState::Warning => Phase::Warning,
            RecoveryObservationState::Stabilizing => Phase::Stabilizing,
            RecoveryObservationState::Recovered => Phase::Recovered,
            RecoveryObservationState::Unavailable => Phase::Unavailable,
            RecoveryObservationState::Blocked => Phase::Blocked,
        }
    }
}

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
