use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rama::futures::async_stream::stream_fn;
use rama::http::Body;
use rama::http::Response;
use rama::http::StatusCode;
use rama::http::header::CACHE_CONTROL;
use rama::http::header::CONTENT_TYPE;
use rama::http::service::web::response::{DatastarScript, IntoResponse};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::ServerState;

const MONITOR_EVENT_CAPACITY: usize = 256;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MonitorEvent {
    pub at_ms: u128,
    pub kind: MonitorEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MonitorEventKind {
    ServerStarted,
    TriggerAccepted,
    ProcessStarted,
    TriggerCompleted,
    TriggerFailed,
    TriggerTimedOut,
    ProcessCleanedUp,
}

#[derive(Debug)]
pub(crate) struct MonitorState {
    started_at: Instant,
    tx: broadcast::Sender<MonitorEvent>,
}

impl MonitorState {
    pub(crate) fn new() -> Self {
        let (tx, _rx) = broadcast::channel(MONITOR_EVENT_CAPACITY);
        Self {
            started_at: Instant::now(),
            tx,
        }
    }

    pub(crate) fn publish(
        &self,
        kind: MonitorEventKind,
        conversation_id: Option<String>,
        process_id: Option<String>,
        detail: Option<String>,
    ) {
        let _ = self.tx.send(MonitorEvent {
            at_ms: self.started_at.elapsed().as_millis(),
            kind,
            conversation_id,
            process_id,
            detail,
        });
    }

    fn subscribe(&self) -> broadcast::Receiver<MonitorEvent> {
        self.tx.subscribe()
    }

    fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }
}

pub(crate) fn page_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .body(Body::from(MONITOR_HTML))
        .unwrap()
}

pub(crate) fn events_response(state: Arc<ServerState>) -> Response {
    let mut rx = state.monitor.subscribe();
    let snapshot = render_snapshot(&state);

    let stream = stream_fn(async move |mut yielder| {
        yielder
            .yield_item(sse_patch("monitor-summary", &snapshot))
            .await;
        yielder
            .yield_item(sse_append_log("monitor-log", "monitor connected"))
            .await;

        loop {
            match rx.recv().await {
                Ok(event) => {
                    yielder
                        .yield_item(sse_patch("monitor-summary", &render_snapshot(&state)))
                        .await;
                    yielder
                        .yield_item(sse_append_log("monitor-log", &render_event(&event)))
                        .await;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    yielder
                        .yield_item(sse_append_log(
                            "monitor-log",
                            &format!("monitor lagged; skipped {skipped} events"),
                        ))
                        .await;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap()
}

pub(crate) fn datastar_script_response() -> Response {
    DatastarScript::default().into_response()
}

fn render_snapshot(state: &ServerState) -> String {
    let available = state.semaphore.available_permits();
    let max = state.max_concurrent;
    let active = max.saturating_sub(available);
    format!(
        r#"<section id="monitor-summary">
  <div><strong>Status:</strong> online</div>
  <div><strong>Version:</strong> {}</div>
  <div><strong>Uptime:</strong> {}s</div>
  <div><strong>Active triggers:</strong> {active}/{max}</div>
</section>"#,
        escape_html(chaos_ipc::product::CHAOS_VERSION),
        state.monitor.uptime().as_secs(),
    )
}

fn render_event(event: &MonitorEvent) -> String {
    let mut parts = vec![format!("+{}ms", event.at_ms), format!("{:?}", event.kind)];
    if let Some(conversation_id) = event.conversation_id.as_deref() {
        parts.push(format!("conversation={conversation_id}"));
    }
    if let Some(process_id) = event.process_id.as_deref() {
        parts.push(format!("process={process_id}"));
    }
    if let Some(detail) = event.detail.as_deref() {
        parts.push(detail.to_string());
    }
    parts.join(" ")
}

fn sse_patch(selector_id: &str, html: &str) -> Result<String, Infallible> {
    Ok(format!(
        "event: datastar-patch-elements\ndata: selector #{selector_id}\ndata: mode outer\ndata: elements {}\n\n",
        one_line_html(html),
    ))
}

fn sse_append_log(selector_id: &str, message: &str) -> Result<String, Infallible> {
    let html = format!("<li><time>{}</time> {}</li>", "now", escape_html(message));
    Ok(format!(
        "event: datastar-patch-elements\ndata: selector #{selector_id}\ndata: mode append\ndata: elements {}\n\n",
        one_line_html(&html),
    ))
}

fn one_line_html(input: &str) -> String {
    input.lines().map(str::trim).collect::<Vec<_>>().join("")
}

fn escape_html(input: impl AsRef<str>) -> String {
    input
        .as_ref()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const MONITOR_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Chaos monitor</title>
  <script type="module" src="/assets/datastar.js"></script>
  <style>
    body { margin: 2rem; font: 14px/1.45 system-ui, sans-serif; color: #17202a; background: #f7f8fa; }
    main { max-width: 920px; margin: 0 auto; }
    section, ol { background: white; border: 1px solid #d8dee4; border-radius: 8px; padding: 1rem; }
    #monitor-summary { display: grid; gap: .35rem; margin-bottom: 1rem; }
    #monitor-log { max-height: 60vh; overflow: auto; }
    li { margin: .25rem 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  </style>
</head>
<body>
  <main data-init="@get('/monitor/events')">
    <h1>Chaos monitor</h1>
    <section id="monitor-summary">
      <div><strong>Status:</strong> connecting…</div>
    </section>
    <h2>Events</h2>
    <ol id="monitor-log"></ol>
  </main>
</body>
</html>
"#;
