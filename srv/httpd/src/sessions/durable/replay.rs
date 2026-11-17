//! Commit observations before state publication; replay never drives execution.
use super::*;
use crate::sessions::{EventBatch, MAX_REPLAY_BYTES, ReplayEvent, Window};
use chaos_journald::{ExecutionReplayRequest, ExecutionReplayResponse};

impl Owner {
    async fn stored(&self) -> Result<(Window, Vec<ReplayEvent>), ApiError> {
        let Response::Replay(ExecutionReplayResponse::Batch(batch)) = self
            .store
            .call(Request::Replay {
                request: ExecutionReplayRequest::Read {
                    session_id: self.id,
                    after: 0,
                },
            })
            .await?
        else {
            return Err(unavailable());
        };
        let sizes: Vec<u64> = batch
            .events
            .iter()
            .map(|event| event.event.to_string().len() as u64)
            .collect();
        let window = batch
            .next_cursor
            .checked_add(1)
            .and_then(|next| Window::restored(Some(MAX_REPLAY_BYTES), 1, next, &sizes))
            .filter(|window| {
                batch
                    .events
                    .first()
                    .is_none_or(|event| event.sequence == window.floor())
            })
            .ok_or_else(unavailable)?;
        let events = batch
            .events
            .into_iter()
            .map(|event| ReplayEvent {
                sequence: event.sequence,
                event: event.event,
            })
            .collect();
        Ok((window, events))
    }

    async fn replay(&self, after: u64) -> Result<EventBatch, ApiError> {
        let (window, events) = self.stored().await?;
        EventBatch::read(&window, &events, after, true)
    }
}

impl Session {
    pub(in crate::sessions) async fn read_events(
        &self,
        after: u64,
    ) -> Result<EventBatch, ApiError> {
        match self.ledger.get() {
            Some(owner) => owner.replay(after).await,
            None => self.events(after),
        }
    }

    pub(super) async fn restore_replay(&self) -> Result<(), ApiError> {
        let owner = self.ledger.get().ok_or_else(unavailable)?;
        let (mut window, mut events) = owner.stored().await?;
        if !owner.fenced() {
            let event = serde_json::json!({
                "id": "",
                "msg": {
                    "type": "http_event_omitted",
                    "reason": "daemon_restart_unobserved_events_possible"
                }
            });
            let Response::Replay(ExecutionReplayResponse::Appended { sequence }) = owner
                .write(Request::Replay {
                    request: ExecutionReplayRequest::Append {
                        lease: owner.lease()?,
                        after: window.next_offset() - 1,
                        event,
                    },
                })
                .await?
            else {
                owner.abandon();
                return Err(unavailable());
            };
            if sequence != window.next_offset() {
                owner.abandon();
                return Err(unavailable());
            }
            (window, events) = owner.stored().await?;
        }
        let mut data = self.data.lock().unwrap();
        data.window = window;
        data.events = events.into();
        Ok(())
    }

    pub(in crate::sessions) async fn observe_committed(
        &self,
        event: &chaos_ipc::protocol::Event,
        quiescent: bool,
    ) -> Result<(), ApiError> {
        if let Some(owner) = self.ledger.get() {
            let after = self.data.lock().unwrap().window.next_offset() - 1;
            let response = owner
                .write(Request::Replay {
                    request: ExecutionReplayRequest::Append {
                        lease: owner.lease()?,
                        after,
                        event: super::super::replay_value(event).0,
                    },
                })
                .await?;
            if !matches!(
                response,
                Response::Replay(ExecutionReplayResponse::Appended { sequence })
                    if sequence == after + 1
            ) {
                owner.abandon();
                return Err(unavailable());
            }
        }
        // Only the single queue supervisor may call this; concurrent HTTP
        // readers see either the committed prefix or a later committed prefix.
        self.observe_with_activity(event, quiescent);
        Ok(())
    }
}
