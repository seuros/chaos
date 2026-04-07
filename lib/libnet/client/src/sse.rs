use crate::error::StreamError;
use crate::transport::ByteStream;
use rama::futures::StreamExt;
use rama::http::sse::EventStream;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio::time::timeout;

/// Minimal SSE helper that forwards raw `data:` frames as UTF-8 strings.
///
/// Errors and idle timeouts are sent as `Err(StreamError)` before the task exits.
pub fn sse_stream(
    stream: ByteStream,
    idle_timeout: Duration,
    tx: mpsc::Sender<Result<String, StreamError>>,
) {
    tokio::spawn(async move {
        let stream = stream.map(|res| res.map_err(|e| StreamError::Stream(e.to_string())));
        let mut stream = EventStream::<_, String>::new(stream);

        loop {
            match timeout(idle_timeout, stream.next()).await {
                Ok(Some(Ok(ev))) => {
                    if tx
                        .send(Ok(ev.data().cloned().unwrap_or_default()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(Some(Err(e))) => {
                    let _ = tx.send(Err(StreamError::Stream(e.to_string()))).await;
                    return;
                }
                Ok(None) => {
                    let _ = tx
                        .send(Err(StreamError::Stream(
                            "stream closed before completion".into(),
                        )))
                        .await;
                    return;
                }
                Err(_) => {
                    let _ = tx.send(Err(StreamError::Timeout)).await;
                    return;
                }
            }
        }
    });
}
