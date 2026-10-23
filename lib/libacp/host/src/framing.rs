use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use futures::{Sink, SinkExt, Stream, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

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
    .filter_map(move |line| {
        let state = state.clone();
        async move {
            let result = (|| {
                let line = line.map_err(|error| transport_error(error, &state))?;
                let frame: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
                if !frame.is_object() {
                    return Err(io::Error::other("ACP requires individual JSON objects"));
                }
                if frame.get("id").is_none() && frame.get("method").is_some() {
                    // Unknown notifications have no responder to release an admission
                    // credit. Discard them here, before any SDK queues. Known
                    // cancellation notifications do not create unbounded native work.
                    return Ok(matches!(
                        frame["method"].as_str(),
                        Some("session/cancel" | "$/cancel_request")
                    )
                    .then_some(line));
                }
                // Requests and replies retain their credit until the SDK responds or
                // consumes the matching reply. Unknown replies still fail boundedly.
                if state.inbound.fetch_add(1, Ordering::AcqRel) >= state.limits.pending_requests {
                    state.fail("ACP inbound admission limit exceeded");
                    return Err(io::Error::other("ACP inbound admission limit exceeded"));
                }
                Ok(Some(line))
            })();
            result.transpose()
        }
    })
}

pub(crate) fn outgoing<W: AsyncWrite + Unpin>(
    writer: W,
    state: Arc<State>,
) -> impl Sink<String, Error = io::Error> {
    let transport_state = state.clone();
    <FramedWrite<W, LinesCodec> as SinkExt<String>>::sink_map_err(
        FramedWrite::new(
            writer,
            LinesCodec::new_with_max_length(state.limits.max_message_bytes),
        ),
        move |error| transport_error(error, &transport_state),
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

fn transport_error(error: LinesCodecError, state: &State) -> io::Error {
    match error {
        LinesCodecError::Io(error) => {
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
            ) {
                // A full peer disconnect may reach the writer before reader EOF.
                // Still run and confirm native cleanup before reporting success.
                state.peer_disconnected.store(true, Ordering::Release);
                state.close();
            }
            error
        }
        error => io::Error::other(error),
    }
}
