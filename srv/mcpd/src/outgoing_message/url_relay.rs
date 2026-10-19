//! Bounded, session/process/source-scoped URL id translation across tool calls.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use chaos_ipc::ProcessId;

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    session: String,
    process: ProcessId,
    server: String,
    source: String,
}

struct Entry {
    id: String,
    created: Instant,
}

#[derive(Default)]
pub(crate) struct UrlElicitations(Mutex<HashMap<Key, Entry>>);

impl UrlElicitations {
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
            id: None,
            retained: false,
        }
    }

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
        let entry = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key)?;
        (entry.created.elapsed() < Duration::from_secs(3600)).then_some(entry.id)
    }
}

pub(crate) struct Reservation {
    urls: Arc<UrlElicitations>,
    key: Key,
    id: Option<String>,
    retained: bool,
}

impl Reservation {
    /// A refused reservation returns an invalid URL to the host API, which
    /// rejects the request before any transport write.
    pub(crate) fn register(&mut self, id: &str) -> bool {
        let mut entries = self
            .urls
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, value| value.created.elapsed() < Duration::from_secs(3600));
        if entries.contains_key(&self.key)
            || entries.len() >= 1024
            || entries
                .keys()
                .filter(|key| key.session == self.key.session)
                .count()
                >= 128
        {
            return false;
        }
        self.id = Some(id.into());
        entries.insert(
            self.key.clone(),
            Entry {
                id: id.into(),
                created: Instant::now(),
            },
        );
        true
    }

    pub(crate) fn retain(&mut self) {
        self.retained = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.retained
            && let Some(id) = &self.id
        {
            let mut entries = self
                .urls
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries.get(&self.key).is_some_and(|entry| &entry.id == id) {
                entries.remove(&self.key);
            }
        }
    }
}
