use std::sync::{Arc, PoisonError};

use crate::protocol;
use crate::protocol::schema::v1::SessionId;
use crate::state::{Slot, State, error};

/// Own the admission gate before spawning the asynchronous close operation.
/// Losing this operation leaves native termination uncertain, so fail closed.
pub(crate) struct Closing {
    state: Arc<State>,
    slot: Arc<Slot>,
    id: SessionId,
    armed: bool,
}

impl Closing {
    pub fn begin(state: Arc<State>, id: SessionId) -> protocol::Result<Self> {
        let slot = state.find(&id)?;
        slot.lock().begin_close()?;
        Ok(Self {
            state,
            slot,
            id,
            armed: true,
        })
    }

    pub async fn finish(mut self) -> protocol::Result<()> {
        let state = &self.state;
        let slot = &self.slot;
        let cancel = slot.lock().request_cancel();
        if let Some(turn) = cancel {
            let result =
                tokio::time::timeout(state.limits.shutdown_timeout, slot.native.cancel()).await;
            turn.cancel_finished();
            if !matches!(result, Ok(Ok(()))) {
                return Err(error("ACP native cancellation is unconfirmed"));
            }
        }
        let idle = async {
            loop {
                let released = slot.released.notified();
                tokio::pin!(released);
                released.as_mut().enable();
                if !slot.lock().occupied() {
                    break;
                }
                released.await;
            }
        };
        tokio::time::timeout(state.limits.shutdown_timeout, idle)
            .await
            .map_err(|_| error("ACP session did not stop"))?;
        state.shutdown_slot(slot).await?;
        slot.invalidate();
        let mut sessions = state
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if sessions
            .get(&self.id)
            .is_some_and(|current| Arc::ptr_eq(current, slot))
        {
            sessions.remove(&self.id);
        }
        self.armed = false;
        Ok(())
    }
}

impl Drop for Closing {
    fn drop(&mut self) {
        if self.armed {
            self.state.fail("ACP session close is unconfirmed");
        }
    }
}
