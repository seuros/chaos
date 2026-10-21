use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use protocol::Responder;
use protocol::schema::v1::{RequestPermissionOutcome, RequestPermissionResponse, SessionId};
use tokio::sync::watch;

use crate::protocol;

/// Serialize cancellation with response submission, not with application policy.
#[derive(Clone, Debug, Default)]
pub(crate) struct PendingPermissions {
    sessions: Arc<Mutex<HashMap<SessionId, Vec<watch::Sender<bool>>>>>,
}

impl PendingPermissions {
    pub fn register(&self, session: SessionId) -> PendingPermission {
        let (cancel, _) = watch::channel(false);
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(session.clone())
            .or_default()
            .push(cancel.clone());
        PendingPermission {
            pending: self.clone(),
            session,
            cancel,
        }
    }

    pub fn cancel_and_send<T>(&self, session: &SessionId, send: impl FnOnce() -> T) -> T {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(requests) = sessions.get(session) {
            for cancel in requests {
                cancel.send_replace(true);
            }
        }
        // An approval already queued before this lock is not pending. No
        // outstanding application decision can queue an approval after it.
        send()
    }
}

pub(crate) struct PendingPermission {
    pending: PendingPermissions,
    session: SessionId,
    cancel: watch::Sender<bool>,
}

impl PendingPermission {
    pub async fn cancelled(&self) {
        let mut receiver = self.cancel.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    pub fn respond(
        &self,
        responder: Responder<RequestPermissionResponse>,
        result: protocol::Result<RequestPermissionResponse>,
    ) -> protocol::Result<()> {
        let _sessions = self
            .pending
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let result = if *self.cancel.borrow() {
            Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Cancelled,
            ))
        } else {
            result
        };
        responder.respond_with_result(result)
    }
}

impl Drop for PendingPermission {
    fn drop(&mut self) {
        let mut sessions = self
            .pending
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(requests) = sessions.get_mut(&self.session) {
            requests.retain(|cancel| !cancel.same_channel(&self.cancel));
            if requests.is_empty() {
                sessions.remove(&self.session);
            }
        }
    }
}
