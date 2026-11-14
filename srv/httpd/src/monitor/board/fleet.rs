//! How other processes and machines change the board.

use chaos_kern::activity::{Activity, ActivityEvent};

use super::{Board, Change, EXITED_KEEP_MS, Origin, Session};

impl Board {
    /// An event from another process. Its session carries the origin before
    /// the event folds, so the event counts for the right machine.
    pub(in crate::monitor) fn apply_remote(
        &mut self,
        origin: Origin,
        event: &ActivityEvent,
    ) -> Vec<Change> {
        if !matches!(
            event.activity,
            Activity::Machine { .. } | Activity::Limits { .. }
        ) {
            if !self.sessions.contains_key(&event.process_id) {
                self.make_room();
            }
            self.sessions
                .entry(event.process_id)
                .or_insert_with(|| Session::new(event.at_ms))
                .origin = Some(origin);
        }
        let mut changes = self.apply(event);
        changes.push(Change::SessionAdded);
        changes.dedup();
        changes
    }

    /// The machines this pane knows: this one, then the others.
    pub(in crate::monitor) fn hosts(&self) -> Vec<String> {
        let local = chaos_kern::activity::fleet::local_host().to_string();
        let mut hosts = vec![local.clone()];
        let others = self
            .elsewhere
            .iter()
            .map(|origin| origin.host.clone())
            .chain(self.sessions.values().map(|s| s.host().to_string()));
        for host in others {
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
        hosts[1..].sort();
        hosts
    }

    /// The board as one machine sees it.
    pub(in crate::monitor) fn only(&self, host: &str) -> Board {
        let mut view = self.clone();
        view.sessions.retain(|_, session| session.host() == host);
        view.feed.retain(|entry| entry.host == host);
        view.stations.retain(|(h, _), _| h == host);
        view.route.retain(|(h, ..)| h == host);
        view.elsewhere.retain(|origin| origin.host == host);
        view
    }

    pub(in crate::monitor) fn joined(&mut self, origin: Origin) -> Vec<Change> {
        if !self.elsewhere.contains(&origin) {
            self.elsewhere.push(origin);
        }
        vec![Change::Fleet]
    }

    /// A process exited. Its sessions stay a while, marked exited, so a run
    /// that finished before anyone looked can still be read.
    pub(in crate::monitor) fn left(&mut self, origin: Origin, now_ms: i64) -> Vec<Change> {
        self.elsewhere.retain(|o| *o != origin);
        for session in self.sessions.values_mut() {
            if session.origin.as_ref() == Some(&origin) && session.exited_at_ms.is_none() {
                session.exited_at_ms = Some(now_ms);
                session.working = false;
                session.thinking = false;
                session.calls.clear();
                session.asks.clear();
                session.waiting_on.clear();
            }
        }
        self.prune(now_ms);
        vec![Change::Fleet, Change::SessionAdded, Change::Map]
    }

    /// Forget sessions whose process exited long enough ago.
    pub(in crate::monitor) fn prune(&mut self, now_ms: i64) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|_, session| {
            session
                .exited_at_ms
                .is_none_or(|at| now_ms - at < EXITED_KEEP_MS)
        });
        self.sessions.len() != before
    }
}
