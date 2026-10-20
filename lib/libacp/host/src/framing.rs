use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use futures::{Sink, SinkExt, Stream, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

use crate::protocol::schema::v1::SessionId;
use crate::state::State;

pub(crate) fn incoming<R: AsyncRead + Unpin>(
    reader: R,
    state: Arc<State>,
) -> impl Stream<Item = io::Result<String>> {
    FramedRead::new(
        reader,
        LinesCodec::new_with_max_length(state.limits.max_message_bytes),
    )
    .map(move |line| {
        let line = line.map_err(io::Error::other)?;
        let frame: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        if !frame.is_object() {
            return Err(io::Error::other("ACP requires individual JSON objects"));
        }
        // Every frame needs a credit, including malformed/unknown replies.
        // SDK callbacks or the actual outgoing responder release credits;
        // unread/unsupported floods cannot fill its internal channels forever.
        if state.inbound.fetch_add(1, Ordering::AcqRel) >= state.limits.pending_requests {
            state.fail("ACP inbound admission limit exceeded");
            return Err(io::Error::other("ACP inbound admission limit exceeded"));
        }
        Ok(line)
    })
}

pub(crate) fn outgoing<W: AsyncWrite + Unpin>(
    writer: W,
    state: Arc<State>,
) -> impl Sink<String, Error = io::Error> {
    <FramedWrite<W, LinesCodec> as SinkExt<String>>::sink_map_err(
        FramedWrite::new(
            writer,
            LinesCodec::new_with_max_length(state.limits.max_message_bytes),
        ),
        io::Error::other,
    )
    .with(move |line: String| {
        let state = state.clone();
        async move {
            if line.len() > state.limits.max_message_bytes {
                state.fail("ACP outgoing frame exceeds limit");
                return Err(io::Error::other("ACP outgoing frame exceeds limit"));
            }
            let frame: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
            if frame.get("method").is_none() {
                state.acknowledge_inbound();
            } else if frame["method"] == "session/update"
                && let Some(id) = frame["params"]["sessionId"].as_str()
                && let Some(slot) = state
                    .sessions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&SessionId::new(id))
            {
                let _ = slot
                    .queued
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                        Some(n.saturating_sub(1))
                    });
                slot.queue_changed.notify_one();
            }
            Ok(line)
        }
    })
}
