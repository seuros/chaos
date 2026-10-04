use state_machines::state_machine;
use std::time::{Duration, Instant};

use super::{JOURNAL_LEASE_REFRESH_INTERVAL, JOURNAL_LEASE_TTL};

#[derive(Debug)]
struct LeaseWindow {
    warning_at: Instant,
}

#[derive(Debug)]
struct Renewal {
    due: Instant,
}

state_machine! {
    name: WriterLease,
    dynamic: true,
    initial: Confirmed,
    states: [
        superstate Owned(LeaseWindow) {
            state Confirmed(Renewal), state Unconfirmed, state Expired
        },
        Fenced,
    ],
    final_states: [Fenced],
    events {
        refresh {
            transition: { from: Confirmed, to: Unconfirmed }
            transition: { from: Unconfirmed, internal: true }
            transition: { from: Expired, internal: true }
        }
        confirm {
            payload: Instant,
            transition: { from: Unconfirmed, to: Confirmed, data: renewal }
        }
        expire { transition: { from: Owned, to: Expired } }
        reacquire { transition: { from: Expired, to: Unconfirmed } }
        fence {
            transition: { from: Owned, to: Fenced }
        }
    }
}

impl<C, S> WriterLease<C, S> {
    fn renewal(&self, confirmed_at: &mut Instant) -> Renewal {
        Renewal {
            due: *confirmed_at + JOURNAL_LEASE_REFRESH_INTERVAL,
        }
    }
}

pub(super) struct Lease {
    machine: DynamicWriterLease<()>,
}

impl Default for Lease {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

impl Lease {
    pub(super) fn new(confirmed_at: Instant) -> Self {
        Self {
            machine: WriterLease::new(())
                .with_owned_data(LeaseWindow {
                    warning_at: confirmed_at + JOURNAL_LEASE_TTL - Duration::from_secs(5),
                })
                .with_confirmed_data(Renewal {
                    due: confirmed_at + JOURNAL_LEASE_REFRESH_INTERVAL,
                })
                .into_dynamic(),
        }
    }

    pub(super) fn confirmed(&self) -> bool {
        self.machine.current_state() == WriterLeaseState::Confirmed
    }
    pub(super) fn fenced(&self) -> bool {
        self.machine.is_finished()
    }
    pub(super) fn needs_reacquire(&self) -> bool {
        self.machine.current_state() == WriterLeaseState::Expired
    }
    pub(super) fn renewal_due(&self, now: Instant) -> bool {
        self.machine
            .confirmed_data()
            .is_none_or(|data| now >= data.due)
    }

    pub(super) fn warning_deadline(&self) -> Option<Instant> {
        self.machine.owned_data().map(|data| data.warning_at)
    }

    pub(super) fn at_risk(&self, now: Instant) -> bool {
        !self.confirmed()
            || self
                .warning_deadline()
                .is_some_and(|deadline| now >= deadline)
    }

    pub(super) fn apply(&mut self, event: WriterLeaseEvent) {
        if self.fenced() && matches!(event, WriterLeaseEvent::Fence) {
            return;
        }
        let confirmed_at = match &event {
            WriterLeaseEvent::Confirm(at) => Some(*at),
            _ => None,
        };
        assert!(
            self.machine.handle(event).is_ok(),
            "journal lease operation respects ownership"
        );
        if let Some(at) = confirmed_at {
            self.machine
                .owned_data_mut()
                .unwrap_or_else(|| unreachable!("confirmed lease owns expiry"))
                .warning_at = at + JOURNAL_LEASE_TTL - Duration::from_secs(5);
        }
    }

    #[cfg(test)]
    pub(super) fn set_confirmed_at(&mut self, at: Instant) {
        self.machine
            .confirmed_data_mut()
            .unwrap_or_else(|| unreachable!("test ages a confirmed lease"))
            .due = at + JOURNAL_LEASE_REFRESH_INTERVAL;
        self.machine
            .owned_data_mut()
            .unwrap_or_else(|| unreachable!("test lease owns expiry"))
            .warning_at = at + JOURNAL_LEASE_TTL - Duration::from_secs(5);
    }
}

#[cfg(test)]
mod tests;
