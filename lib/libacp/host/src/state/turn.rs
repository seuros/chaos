use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::Slot;
use crate::AgentSession;
use crate::lifecycle::TurnState;

#[derive(Debug)]
pub(crate) struct Turn {
    pub cancel: CancellationToken,
    pub cancel_done: CancellationToken,
    lifecycle: Mutex<TurnState>,
}

impl Turn {
    pub fn new_operation(prompt: bool) -> Arc<Self> {
        Arc::new(Self {
            lifecycle: Mutex::new(TurnState::new(prompt)),
            cancel: CancellationToken::new(),
            cancel_done: CancellationToken::new(),
        })
    }

    pub fn is_live(&self) -> bool {
        self.lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_live()
    }

    pub fn request_cancel(&self) -> bool {
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        lifecycle.request_cancel()
    }

    pub async fn cancel_native(&self, native: &dyn AgentSession, limit: Duration) -> bool {
        let result = tokio::time::timeout(limit, async {
            let interrupted = native.interrupt().await;
            self.cancel.cancel();
            interrupted?;
            native.cancel().await
        })
        .await;
        self.cancel.cancel();
        self.cancel_finished();
        matches!(result, Ok(Ok(())))
    }

    pub fn cancel_finished(&self) {
        self.lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancel_finished();
        self.cancel_done.cancel();
    }
}

pub(crate) struct TurnGuard {
    pub slot: Arc<Slot>,
    pub turn: Arc<Turn>,
}

impl TurnGuard {
    pub fn complete(&self) -> bool {
        let _session = self.slot.lock();
        self.turn
            .lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .complete()
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        {
            let mut session = self.slot.lock();
            let uncertain = self
                .turn
                .lifecycle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .abandon();
            if uncertain {
                session.invalidate();
            }
            session.release(&self.turn);
        }
        self.turn.cancel.cancel();
        self.slot.released.notify_waiters();
    }
}
