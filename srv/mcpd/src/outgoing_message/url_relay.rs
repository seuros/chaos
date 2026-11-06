//! Bounded, session/process/source-scoped URL id translation across tool calls.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use chaos_ipc::ProcessId;
use dedup_machines::{Attempt, Claim, ClaimId, HashDedup};

const RETENTION: Duration = Duration::from_secs(3600);
const TOTAL_CAP: usize = 1024;
const SESSION_CAP: usize = 128;

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    session: String,
    process: ProcessId,
    server: String,
    source: String,
}

impl Key {
    fn attempt(&self) -> Attempt<Self> {
        let mut hasher = DefaultHasher::new();
        self.session.hash(&mut hasher);
        Attempt::new(self.clone()).partition(hasher.finish())
    }
}

fn uptime() -> Duration {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed()
}

pub(crate) struct UrlElicitations(Mutex<HashDedup<Key, String>>);

impl Default for UrlElicitations {
    fn default() -> Self {
        Self(Mutex::new(
            HashDedup::with_capacity(TOTAL_CAP).with_partition_cap(SESSION_CAP),
        ))
    }
}

impl UrlElicitations {
    fn ledger(&self) -> MutexGuard<'_, HashDedup<Key, String>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn reservation(
        self: &Arc<Self>,
        session: String,
        process: ProcessId,
        server: String,
        source: String,
    ) -> Reservation {
        Reservation {
            urls: self.clone(),
            key: Key {
                session,
                process,
                server,
                source,
            },
            claim: None,
            id: None,
        }
    }

    /// Take the outer id a retained reservation stored for this key.
    pub(crate) fn complete(
        &self,
        session: &str,
        process: ProcessId,
        server: String,
        source: String,
    ) -> Option<String> {
        let key = Key {
            session: session.into(),
            process,
            server,
            source,
        };
        let mut ledger = self.ledger();
        let id = match ledger.admit(key.attempt(), uptime(), RETENTION) {
            Claim::Completed(id) => id.clone(),
            Claim::Acquired(claim) => {
                let _ = ledger.abandon(claim);
                return None;
            }
            _ => return None,
        };
        ledger.expire_where(|candidate| *candidate == key);
        Some(id)
    }
}

pub(crate) struct Reservation {
    urls: Arc<UrlElicitations>,
    key: Key,
    claim: Option<ClaimId>,
    id: Option<String>,
}

impl Reservation {
    /// A refused reservation returns an invalid URL to the host API, which
    /// rejects the request before any transport write.
    pub(crate) fn register(&mut self, id: &str) -> bool {
        let mut ledger = self.urls.ledger();
        match ledger.admit(self.key.attempt(), uptime(), RETENTION) {
            Claim::Acquired(claim) => {
                self.claim = Some(claim);
                self.id = Some(id.into());
                true
            }
            _ => false,
        }
    }

    pub(crate) fn retain(&mut self) {
        if let (Some(claim), Some(id)) = (self.claim.take(), self.id.take()) {
            let _ = self.urls.ledger().complete_with(claim, id, uptime());
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            let _ = self.urls.ledger().abandon(claim);
        }
    }
}
