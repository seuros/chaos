use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use rama::futures::async_stream::stream_fn;
use rama::http::{Body, HeaderValue, Response};

use super::{Session, SessionStatus};

pub(super) fn response(
    session: Arc<Session>,
    daemon_id: String,
    mut cursor: u64,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Response {
    // Subscribe BEFORE reading replay, closing the snapshot/subscription race.
    let mut changed = session.changed.subscribe();
    let stream = stream_fn(async move |mut yielder| {
        loop {
            changed.borrow_and_update();
            let batch = match session.read_events(cursor).await {
                Ok(batch) => batch,
                Err(_) => {
                    // Never silently fall back to an in-memory prefix when
                    // durable replay is unavailable. Reconnect at the same ID.
                    yielder
                        .yield_item(Ok::<_, Infallible>(
                            "event: replay_unavailable\ndata: {\"reconnect\":true}\n\n".to_owned(),
                        ))
                        .await;
                    break;
                }
            };
            if batch.gap {
                yielder
                    .yield_item(Ok::<_, Infallible>(format!(
                        "event: gap\ndata: {{\"after\":{cursor},\"next_cursor\":{}}}\n\n",
                        batch.next_cursor
                    )))
                    .await;
            }
            for event in batch.events {
                yielder
                    .yield_item(Ok(format!(
                        "id: {}\nevent: kernel\ndata: {}\n\n",
                        event.sequence, event.event
                    )))
                    .await;
            }
            cursor = batch.next_cursor;
            let snapshot = session.snapshot();
            let shutting_down = *shutdown.borrow_and_update();
            let terminal = shutting_down
                || matches!(
                    snapshot.status,
                    SessionStatus::Closed | SessionStatus::Failed
                );
            yielder
                .yield_item(Ok(format!(
                    "event: session\ndata: {}\n\n",
                    serde_json::json!({
                        "daemon_id": daemon_id,
                        "session": snapshot,
                        "shutting_down": shutting_down,
                    })
                )))
                .await;
            if terminal {
                break;
            }
            tokio::select! {
                change = changed.changed() => {
                    if change.is_err() { break; }
                }
                stopping = shutdown.changed() => {
                    if stopping.is_err() { break; }
                }
                _ = tokio::time::sleep(Duration::from_secs(15)) => {
                    yielder.yield_item(Ok(": keepalive\n\n".to_string())).await;
                }
            }
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}
