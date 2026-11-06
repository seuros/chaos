use lease_machines::{Cadence, FencingToken, Holder, HolderState};
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

const FIRST_TOKEN: FencingToken = match FencingToken::new(1) {
    Some(token) => token,
    None => panic!("1 is a valid fencing token"),
};

fn fencing_token(raw: &str, previous: Option<FencingToken>) -> FencingToken {
    raw.parse::<u64>()
        .ok()
        .and_then(FencingToken::new)
        .or_else(|| FencingToken::new(previous.map_or(1, |token| token.value().saturating_add(1))))
        .unwrap_or(FIRST_TOKEN)
}

#[derive(Debug)]
pub(super) struct Lease {
    epoch: Instant,
    holder: Holder,
}

#[cfg(test)]
impl Default for Lease {
    fn default() -> Self {
        Self::new("", Instant::now())
    }
}

impl Lease {
    pub(super) fn new(token: &str, confirmed_at: Instant) -> Self {
        Self {
            epoch: confirmed_at,
            holder: Holder::new(fencing_token(token, None), Duration::ZERO, CADENCE),
        }
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

    pub(super) fn refresh(&mut self, now: Instant) {
        self.holder.refresh(self.offset(now));
    }

    pub(super) fn confirm(&mut self, token: &str, at: Instant) -> bool {
        let token = fencing_token(token, Some(self.holder.token()));
        self.holder.confirm(token, self.offset(at))
    }

    pub(super) fn expire(&mut self) {
        self.holder.expire();
    }

    pub(super) fn reacquire(&mut self, token: &str, at: Instant) -> bool {
        let token = fencing_token(token, Some(self.holder.token()));
        self.holder.reacquire(token, self.offset(at))
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
