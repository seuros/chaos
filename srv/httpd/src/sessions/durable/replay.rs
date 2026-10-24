//! Commit observations before state publication; replay never drives execution.
use super::*;
use chaos_journald::{ExecutionReplayRequest, ExecutionReplayResponse};

impl Owner {
    async fn replay(&self, after: u64) -> Result<super::super::EventBatch, ApiError> {
        let Response::Replay(ExecutionReplayResponse::Batch(mut batch)) = self
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
        if after > batch.next_cursor {
            return Err(ApiError::bad_request(
                "event cursor is ahead of this session",
            ));
        }
        batch.gap = batch
            .events
            .first()
            .map_or(after < batch.next_cursor, |event| {
                after < event.sequence - 1
            });
        batch.events.retain(|event| event.sequence > after);
        Ok(super::super::EventBatch {
            events: batch
                .events
                .into_iter()
                .map(|event| {
                    let bytes = event.event.to_string().len();
                    super::super::ReplayEvent {
                        sequence: event.sequence,
                        event: event.event,
                        bytes,
                    }
                })
                .collect(),
            gap: batch.gap,
            next_cursor: batch.next_cursor,
            observation_only: true,
            durable: true,
        })
    }
}

impl Session {
    pub(in crate::sessions) async fn read_events(
        &self,
        after: u64,
    ) -> Result<super::super::EventBatch, ApiError> {
        match self.ledger.get() {
            Some(owner) => owner.replay(after).await,
            None => self.events(after),
        }
    }

    pub(super) async fn restore_replay(&self) -> Result<(), ApiError> {
        let owner = self.ledger.get().ok_or_else(unavailable)?;
        let mut batch = owner.replay(0).await?;
        if owner.lease.lock().unwrap().is_some() {
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
                        after: batch.next_cursor,
                        event,
                    },
                })
                .await?
            else {
                owner.abandon();
                return Err(unavailable());
            };
            if sequence != batch.next_cursor + 1 {
                owner.abandon();
                return Err(unavailable());
            }
            batch = owner.replay(0).await?;
        }
        let mut data = self.data.lock().unwrap();
        data.sequence = batch.next_cursor;
        data.replay_bytes = batch.events.iter().map(|event| event.bytes).sum();
        data.events = batch.events.into();
        Ok(())
    }

    pub(in crate::sessions) async fn observe_committed(
        &self,
        event: &chaos_ipc::protocol::Event,
        quiescent: bool,
    ) -> Result<(), ApiError> {
        if let Some(owner) = self.ledger.get() {
            let after = self.data.lock().unwrap().sequence;
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
