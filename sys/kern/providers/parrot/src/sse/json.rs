//! Shared transport loop for adapters whose SSE data frames contain JSON.

use chaos_abi::{AbiError, TurnEvent};
use rama::bytes::Bytes;
use rama::error::BoxError;
use rama::futures;
use rama::futures::StreamExt;
use rama::http::sse::EventStream;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

pub(crate) async fn process_json_sse<S, E, F>(
    data_stream: S,
    idle_timeout: Duration,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
    mut parse: F,
) -> Result<(), AbiError>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<BoxError> + std::fmt::Display,
    F: FnMut(&Value, &mut String) -> Result<Vec<TurnEvent>, AbiError>,
{
    let mut stream = EventStream::<_, String>::new(data_stream);
    let mut response_id = String::new();

    loop {
        let sse = match timeout(idle_timeout, stream.next()).await {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(err))) => return Err(AbiError::Stream(err.to_string())),
            Ok(None) => break,
            Err(_) => return Err(AbiError::Stream("idle timeout waiting for SSE".to_string())),
        };
        let data = match sse.data() {
            Some(data) => data.trim(),
            None => continue,
        };
        if data == "[DONE]" {
            break;
        }
        let json = match serde_json::from_str::<Value>(data) {
            Ok(json) => json,
            Err(_) => continue,
        };
        for event in parse(&json, &mut response_id)? {
            let completed = matches!(&event, TurnEvent::Completed { .. });
            if tx.send(Ok(event)).await.is_err() || completed {
                return Ok(());
            }
        }
    }

    // Neither [DONE] nor EOF supplies usage. Explicit completion returns above,
    // so this fallback is sent exactly once; a disconnected consumer needs no error.
    let _ = tx
        .send(Ok(TurnEvent::Completed {
            response_id,
            token_usage: None,
        }))
        .await;
    Ok(())
}

#[cfg(test)]
mod tests;
