use std::sync::Arc;
use std::time::{Duration, Instant};

use rama::error::BoxError;
use rama::futures::async_stream::stream_fn;
use rama::http::Body;
use rama::http::HeaderValue;
use rama::http::Response;
use rama::http::header::CACHE_CONTROL;
use rama::http::header::CONTENT_TYPE;
use rama::http::service::web::response::{DatastarScript, IntoResponse, Sse};
use rama::http::sse::Event;
use rama::http::sse::datastar::{ElementPatchMode, PatchElements};
use rama::utils::str::NonEmptyStr;
use rama::utils::str::non_empty_str;
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
    let mut response = Response::new(Body::from(MONITOR_HTML));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(crate) fn events_response(state: Arc<ServerState>) -> Response {
    let mut rx = state.monitor.subscribe();
    let mut activity = chaos_kern::activity::subscribe();
    let snapshot = render_snapshot(&state);

    let stream = stream_fn(async move |mut yielder| {
        yielder.yield_item(summary(&snapshot)).await;
        yielder.yield_item(log_line("monitor connected")).await;

        loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Ok(event) => {
                        yielder.yield_item(summary(&render_snapshot(&state))).await;
                        yielder.yield_item(log_line(&render_event(&event))).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        yielder.yield_item(lagged(skipped)).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                event = activity.recv() => match event {
                    Ok(event) => {
                        yielder.yield_item(activity_line(&event)).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        yielder.yield_item(lagged(skipped)).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    });

    let mut response = Sse::new(stream).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
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

type Patch = Result<Event<PatchElements>, BoxError>;

fn summary(html: &str) -> Patch {
    Ok(PatchElements::new(NonEmptyStr::try_from(html)?).try_into_sse_event()?)
}

fn log_line(message: &str) -> Patch {
    let html = format!("<li>{}</li>", escape_html(message));
    Ok(PatchElements::new(NonEmptyStr::try_from(html)?)
        .with_selector(non_empty_str!("#monitor-log"))
        .with_mode(ElementPatchMode::Append)
        .try_into_sse_event()?)
}

fn lagged(skipped: u64) -> Patch {
    log_line(&format!("monitor lagged; skipped {skipped} events"))
}

fn activity_line(event: &chaos_kern::activity::ActivityEvent) -> Patch {
    log_line(&serde_json::to_string(event)?)
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
    body {
      margin: 2rem; font: 14px/1.45 system-ui, sans-serif; color: #17202a; background: #f7f8fa;
    }
    main { max-width: 920px; margin: 0 auto; }
    section, ol { background: white; border: 1px solid #d8dee4; border-radius: 8px; padding: 1rem; }
    #monitor-summary { display: grid; gap: .35rem; margin-bottom: 1rem; }
    #monitor-log { max-height: 60vh; overflow: auto; }
    li { margin: .25rem 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  </style>
</head>
<body>
  <main>
    <h1>Chaos monitor</h1>
    <form data-on:submit__prevent="@get('/monitor/events', {
      headers: {Authorization: 'Bearer ' + el.elements.namedItem('monitor-token').value},
      filterSignals: {include: /^$/}, retry: 'never'
    })">
      <label>Bearer token
        <input type="password" id="monitor-token" autocomplete="off" required>
      </label>
      <button type="submit">Connect</button>
    </form>
    <p>The token stays in this page, not in URLs or browser storage. Reload to disconnect.</p>
    <section id="monitor-summary">
      <div><strong>Status:</strong> disconnected — enter the server token to connect.</div>
    </section>
    <h2>Events</h2>
    <ol id="monitor-log"></ol>
  </main>
</body>
</html>
"#;

#[cfg(test)]
mod tests;
