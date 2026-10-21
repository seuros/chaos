use std::sync::PoisonError;

use protocol::schema::v1::ClientCapabilities;

use super::{State, invalid};
use crate::{HostError, protocol};

impl State {
    pub fn initialize(&self, capabilities: ClientCapabilities) -> protocol::Result<()> {
        let mut connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !connection.initialize() {
            return Err(invalid("ACP already initialized or connection closed"));
        }
        // Publish negotiated capabilities before another thread can observe Open.
        let _ = self.client_capabilities.set(capabilities);
        Ok(())
    }

    pub fn ready(&self) -> protocol::Result<()> {
        if self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_open()
        {
            Ok(())
        } else {
            Err(invalid("ACP connection must be initialized and open"))
        }
    }

    pub fn close(&self) {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .close();
        self.stop.cancel();
    }

    pub fn fail(&self, reason: &'static str) {
        {
            let mut connection = self
                .connection
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            connection.failure.get_or_insert(reason);
            connection.close();
        }
        self.stop.cancel();
    }

    pub fn failure_reason(&self) -> Option<&'static str> {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failure
    }

    pub async fn cleanup(&self) -> Result<(), HostError> {
        let owner = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .begin_cleanup();
        if !owner {
            // Concurrent cleanup callers observe the same result, not premature success.
            self.cleanup_done.cancelled().await;
            return self.cleanup_result();
        }
        let _guard = Cleanup(self);
        self.stop.cancel();
        let sessions =
            std::mem::take(&mut *self.sessions.lock().unwrap_or_else(PoisonError::into_inner));
        for slot in sessions.values() {
            let mut session = slot.lock();
            session.invalidate();
            if let Some(turn) = session.active() {
                turn.cancel.cancel();
            }
        }
        let cleanup =
            futures::future::join_all(sessions.values().map(|slot| self.shutdown_slot(slot)));
        let confirmed = matches!(
            tokio::time::timeout(self.limits.shutdown_timeout, cleanup).await,
            Ok(results) if results.iter().all(Result::is_ok)
        );
        self.finish_cleanup(confirmed);
        self.cleanup_result()
    }

    fn finish_cleanup(&self, confirmed: bool) {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .finish_cleanup(confirmed);
        self.cleanup_done.cancel();
    }

    fn cleanup_result(&self) -> Result<(), HostError> {
        if self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cleanup_confirmed()
        {
            Ok(())
        } else {
            Err(HostError::ShutdownUnconfirmed)
        }
    }
}

struct Cleanup<'a>(&'a State);

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        // If the owner future is dropped, wake joiners with an uncertain outcome.
        // A confirmed terminal state rejects this event and remains confirmed.
        self.0.finish_cleanup(false);
    }
}
