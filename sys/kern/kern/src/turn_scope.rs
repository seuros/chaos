use std::future::Future;
use std::time::Duration;

use deadline_machines::Deadline;
use deadline_machines::Refusal;
use deadline_machines::Scope;
use deadline_machines::tokio_support::TokioClock;
use tokio::time::Instant;

use crate::exec::ExecExpiration;

#[derive(Clone, Debug)]
pub struct TurnScope {
    clock: TokioClock,
    scope: Scope,
}

impl TurnScope {
    #[must_use]
    pub fn new(clock: TokioClock, scope: Scope) -> Self {
        Self { clock, scope }
    }

    #[must_use]
    pub fn never() -> Self {
        Self::new(TokioClock::new(), Scope::never())
    }

    #[must_use]
    pub fn after(timeout: Duration) -> Self {
        let clock = TokioClock::new();
        Self::new(clock, Scope::new(Deadline::after(clock.now(), timeout)))
    }

    #[must_use]
    pub fn until(at: Instant) -> Self {
        Self::after(at.saturating_duration_since(Instant::now()))
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.scope.remaining(self.clock.now())
    }

    pub(crate) fn derive(&self, timeout: Duration) -> Self {
        Self::new(self.clock, self.scope.derive(self.clock.now(), timeout))
    }

    pub(crate) fn exec_expiration(&self, timeout_ms: Option<u64>) -> ExecExpiration {
        let own = ExecExpiration::from(timeout_ms);
        match (self.remaining(), own.timeout_ms()) {
            (Some(_), Some(ms)) => ExecExpiration::Timeout(
                self.derive(Duration::from_millis(ms))
                    .remaining()
                    .unwrap_or_default(),
            ),
            _ => own,
        }
    }

    pub async fn run<F: Future>(&self, future: F) -> Result<F::Output, Refusal> {
        self.scope.tokio_timeout(&self.clock, future).await
    }
}

#[cfg(test)]
mod tests;
