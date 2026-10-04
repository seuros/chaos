use state_machines::state_machine;
use std::{collections::HashSet, path::PathBuf};
use tokio::time::Instant;

#[derive(Debug)]
pub(super) struct ThrottleClock {
    pub next_allowed_at: Instant,
}

state_machine! {
    name: PathThrottle,
    dynamic: true,
    initial: Idle,
    states: [
        superstate Observing(ThrottleClock) { state Idle, state Pending(HashSet<PathBuf>) },
        Closed,
    ],
    final_states: [Closed],
    events {
        begin {
            payload: Option<HashSet<PathBuf>>,
            transition: { from: Idle, to: Pending, data: own_paths }
        }
        flush { transition: { from: Pending, to: Idle } }
        close { transition: { from: Observing, to: Closed } }
    }
}

impl<C, S> PathThrottle<C, S> {
    fn own_paths(&self, paths: &mut Option<HashSet<PathBuf>>) -> HashSet<PathBuf> {
        paths
            .take()
            .unwrap_or_else(|| unreachable!("begin owns its paths"))
    }
}
