use super::*;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_stream::StreamExt;

/// Simple fake event source for tests; feed events via the handle.
struct FakeEventSource {
    rx: mpsc::UnboundedReceiver<EventResult>,
    tx: mpsc::UnboundedSender<EventResult>,
}

struct FakeEventSourceHandle {
    broker: Arc<EventBroker<FakeEventSource>>,
}

impl FakeEventSource {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { rx, tx }
    }
}

impl Default for FakeEventSource {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEventSourceHandle {
    fn new(broker: Arc<EventBroker<FakeEventSource>>) -> Self {
        Self { broker }
    }

    fn send(&self, event: EventResult) {
        let mut state = self
            .broker
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(source) = state.active_event_source_mut() else {
            return;
        };
        let _ = source.tx.send(event);
    }
}

impl EventSource for FakeEventSource {
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<EventResult>> {
        Pin::new(&mut self.get_mut().rx).poll_recv(cx)
    }
}

fn make_stream(
    broker: Arc<EventBroker<FakeEventSource>>,
    draw_rx: broadcast::Receiver<()>,
    terminal_focused: Arc<AtomicBool>,
) -> TuiEventStream<FakeEventSource> {
    TuiEventStream::new(
        broker,
        draw_rx,
        terminal_focused,
        crate::tui::job_control::SuspendContext::new(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )
}

type SetupState = (
    Arc<EventBroker<FakeEventSource>>,
    FakeEventSourceHandle,
    broadcast::Sender<()>,
    broadcast::Receiver<()>,
    Arc<AtomicBool>,
);

fn setup() -> SetupState {
    let source = FakeEventSource::new();
    let broker = Arc::new(EventBroker::new());
    *broker.state.lock().unwrap() = EventBrokerState::Running(source);
    let handle = FakeEventSourceHandle::new(broker.clone());

    let (draw_tx, draw_rx) = broadcast::channel(1);
    let terminal_focused = Arc::new(AtomicBool::new(true));
    (broker, handle, draw_tx, draw_rx, terminal_focused)
}

pub(crate) async fn event_stream_suite() {
    key_event_skips_unmapped().await;
    draw_and_key_events_yield_both().await;
    lagged_draw_maps_to_draw().await;
    error_or_eof_ends_stream().await;
    resume_wakes_paused_stream().await;
    resume_wakes_pending_stream().await;
    resize_burst_debounces_to_single_draw().await;
}

async fn resize_burst_debounces_to_single_draw() {
    let (broker, handle, _draw_tx, draw_rx, terminal_focused) = setup();
    let mut stream = make_stream(broker, draw_rx, terminal_focused);

    handle.send(Ok(Event::Resize(80, 24)));
    handle.send(Ok(Event::Resize(100, 30)));
    let key = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
    handle.send(Ok(Event::Key(key)));

    assert!(matches!(stream.next().await, Some(TuiEvent::Key(k)) if k == key));
    assert!(matches!(stream.next().await, Some(TuiEvent::Draw)));
    let after = timeout(Duration::from_millis(50), stream.next()).await;
    assert!(after.is_err(), "expected no further events, got {after:?}");
}

async fn key_event_skips_unmapped() {
    let (broker, handle, _draw_tx, draw_rx, terminal_focused) = setup();
    let mut stream = make_stream(broker, draw_rx, terminal_focused);

    handle.send(Ok(Event::FocusLost));
    handle.send(Ok(Event::Key(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
    ))));

    let next = stream.next().await.unwrap();
    match next {
        TuiEvent::Key(key) => {
            assert_eq!(key, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        }
        other => panic!("expected key event, got {other:?}"),
    }
}

async fn draw_and_key_events_yield_both() {
    let (broker, handle, draw_tx, draw_rx, terminal_focused) = setup();
    let mut stream = make_stream(broker, draw_rx, terminal_focused);

    let expected_key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    let _ = draw_tx.send(());
    handle.send(Ok(Event::Key(expected_key)));

    let first = stream.next().await.unwrap();
    let second = stream.next().await.unwrap();

    let mut saw_draw = false;
    let mut saw_key = false;
    for event in [first, second] {
        match event {
            TuiEvent::Draw => {
                saw_draw = true;
            }
            TuiEvent::Key(key) => {
                assert_eq!(key, expected_key);
                saw_key = true;
            }
            other => panic!("expected draw or key event, got {other:?}"),
        }
    }

    assert!(saw_draw && saw_key, "expected both draw and key events");
}

async fn lagged_draw_maps_to_draw() {
    let (broker, _handle, draw_tx, draw_rx, terminal_focused) = setup();
    let mut stream = make_stream(broker, draw_rx.resubscribe(), terminal_focused);

    // Fill channel to force Lagged on the receiver.
    let _ = draw_tx.send(());
    let _ = draw_tx.send(());

    let first = stream.next().await;
    assert!(matches!(first, Some(TuiEvent::Draw)));
}

async fn error_or_eof_ends_stream() {
    let (broker, handle, _draw_tx, draw_rx, terminal_focused) = setup();
    let mut stream = make_stream(broker, draw_rx, terminal_focused);

    handle.send(Err(std::io::Error::other("boom")));

    let next = stream.next().await;
    assert!(next.is_none());
}

async fn resume_wakes_paused_stream() {
    let (broker, handle, _draw_tx, draw_rx, terminal_focused) = setup();
    let mut stream = make_stream(broker.clone(), draw_rx, terminal_focused);

    broker.pause_events();

    let task = tokio::spawn(async move { stream.next().await });
    tokio::task::yield_now().await;

    broker.resume_events();
    let expected_key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
    handle.send(Ok(Event::Key(expected_key)));

    let event = timeout(Duration::from_millis(100), task)
        .await
        .expect("timed out waiting for resumed event")
        .expect("join failed");
    match event {
        Some(TuiEvent::Key(key)) => assert_eq!(key, expected_key),
        other => panic!("expected key event, got {other:?}"),
    }
}

async fn resume_wakes_pending_stream() {
    let (broker, handle, _draw_tx, draw_rx, terminal_focused) = setup();
    let mut stream = make_stream(broker.clone(), draw_rx, terminal_focused);

    let task = tokio::spawn(async move { stream.next().await });
    tokio::task::yield_now().await;

    broker.pause_events();
    broker.resume_events();
    let expected_key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
    handle.send(Ok(Event::Key(expected_key)));

    let event = timeout(Duration::from_millis(100), task)
        .await
        .expect("timed out waiting for resumed event")
        .expect("join failed");
    match event {
        Some(TuiEvent::Key(key)) => assert_eq!(key, expected_key),
        other => panic!("expected key event, got {other:?}"),
    }
}
