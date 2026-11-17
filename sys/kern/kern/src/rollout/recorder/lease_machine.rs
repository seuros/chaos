use lease_machines::{Cadence, FencingToken, Holder, HolderState, Refusal};
use std::time::{Duration, Instant};

use super::{JOURNAL_LEASE_REFRESH_INTERVAL, JOURNAL_LEASE_TTL};

const JOURNAL_LEASE_WARN_BEFORE: Duration = Duration::from_secs(5);

const CADENCE: Cadence = match Cadence::new(
    JOURNAL_LEASE_TTL,
    JOURNAL_LEASE_REFRESH_INTERVAL,
    JOURNAL_LEASE_WARN_BEFORE,
) {
    Some(cadence) => cadence,
    None => panic!("journal lease renewal fits inside its ttl"),
};

fn fencing_token(raw: &str) -> Option<FencingToken> {
    raw.parse::<u64>().ok().and_then(FencingToken::from_raw)
}

#[derive(Debug)]
pub(super) struct Lease {
    epoch: Instant,
    holder: Holder,
}

#[cfg(test)]
impl Default for Lease {
    fn default() -> Self {
        Self::new("1", Instant::now()).expect("1 is a fencing token")
    }
}

impl Lease {
    pub(super) fn new(token: &str, confirmed_at: Instant) -> Option<Self> {
        Some(Self {
            epoch: confirmed_at,
            holder: Holder::new(fencing_token(token)?, Duration::ZERO, CADENCE),
        })
    }

    fn offset(&self, at: Instant) -> Duration {
        at.saturating_duration_since(self.epoch)
    }

    #[cfg(test)]
    pub(super) fn confirmed(&self) -> bool {
        self.holder.state() == HolderState::Confirmed
    }

    pub(super) fn fenced(&self) -> bool {
        self.holder.state() == HolderState::Fenced
    }

    pub(super) fn needs_reacquire(&self) -> bool {
        self.holder.state() == HolderState::Expired
    }

    pub(super) fn renewal_due(&self, now: Instant) -> bool {
        self.holder.renewal_due(self.offset(now))
    }

    pub(super) fn warning_deadline(&self) -> Option<Instant> {
        self.holder
            .warning_deadline()
            .map(|deadline| self.epoch + deadline)
    }

    pub(super) fn at_risk(&self, now: Instant) -> bool {
        self.holder.at_risk(self.offset(now))
    }

    pub(super) fn refresh(&mut self, now: Instant) -> Result<(), Refusal> {
        self.holder.refresh(self.offset(now)).map(|_| ())
    }

    pub(super) fn confirm(&mut self, token: &str, at: Instant) -> bool {
        let at = self.offset(at);
        fencing_token(token).is_some_and(|token| self.holder.confirm(token, at).is_ok())
    }

    pub(super) fn expire(&mut self) -> Result<(), Refusal> {
        self.holder.expire().map(|_| ())
    }

    pub(super) fn reacquire(&mut self, token: &str, at: Instant) -> bool {
        let at = self.offset(at);
        fencing_token(token).is_some_and(|token| self.holder.reacquire(token, at).is_ok())
    }

    pub(super) fn fence(&mut self) {
        self.holder.fence();
    }

    #[cfg(test)]
    pub(super) fn set_confirmed_at(&mut self, at: Instant) {
        self.epoch = at;
        self.holder = Holder::new(self.holder.token(), Duration::ZERO, CADENCE);
    }
}

#[cfg(test)]
mod tests;
