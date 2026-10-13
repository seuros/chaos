use super::*;
use chaos_ipc::protocol::{TurnAbortReason, TurnAbortedEvent, TurnCompleteEvent};
use runtime::{Backend, supervise};
use tokio::sync::mpsc;

fn ready_session() -> (Arc<Session>, mpsc::Receiver<Command>) {
    let manager = SessionManager::new(1, Duration::from_secs(3600));
    let (session, commands) = manager.reserve(None).unwrap();
    session.data.lock().unwrap().status = SessionStatus::Idle;
    (session, commands)
}

fn request(text: &str) -> TurnRequest {
    TurnRequest {
        request: text.into(),
        model: None,
        reasoning_effort: None,
    }
}

fn submit_turn(session: &Session, id: &str, text: &str, capacity: &Arc<Semaphore>) -> TurnSnapshot {
    session
        .submit(id.into(), request(text), capacity, Duration::from_secs(60))
        .unwrap()
}

fn complete(id: &str) -> Event {
    Event {
        id: id.into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: id.into(),
            last_agent_message: Some("done".into()),
        }),
    }
}

#[tokio::test]
async fn retries_are_payload_bound_and_capacity_is_released_only_by_matching_terminal_event() {
    let (session, _commands) = ready_session();
    let capacity = Arc::new(Semaphore::new(1));
    submit_turn(&session, "first", "hello", &capacity);
    assert_eq!(capacity.available_permits(), 0);
    let repeated = submit_turn(&session, "first", "hello", &capacity);
    assert_eq!(repeated.status, TurnStatus::Accepted);
    assert_eq!(
        session
            .submit(
                "first".into(),
                request("different"),
                &capacity,
                Duration::from_secs(60)
            )
            .unwrap_err()
            .code,
        StatusCode::CONFLICT
    );
    session.observe(&complete("unrelated"));
    assert_eq!(capacity.available_permits(), 0);
    session.observe(&complete("first"));
    assert_eq!(capacity.available_permits(), 1);
    assert_eq!(
        session.turn("first").unwrap().result.as_deref(),
        Some("done")
    );
    session.data.lock().unwrap().status = SessionStatus::Closed;
    assert_eq!(
        session
            .submit(
                "first".into(),
                request("hello"),
                &capacity,
                Duration::from_secs(60)
            )
            .unwrap()
            .status,
        TurnStatus::Completed
    );
}

#[tokio::test]
async fn controls_do_not_need_execution_capacity_and_stale_targets_are_rejected() {
    let (session, mut commands) = ready_session();
    let capacity = Arc::new(Semaphore::new(1));
    submit_turn(&session, "first", "hello", &capacity);
    assert_eq!(capacity.available_permits(), 0);
    assert!(matches!(commands.recv().await, Some(Command::Turn(_))));
    assert_eq!(
        session.interrupt("stale".into()).unwrap_err().code,
        StatusCode::CONFLICT
    );
    assert_eq!(
        session.interrupt("first".into()).unwrap().status,
        TurnStatus::Interrupting
    );
    assert_eq!(
        session.interrupt("first".into()).unwrap().status,
        TurnStatus::Interrupting
    );
    assert_eq!(capacity.available_permits(), 0);
    assert!(matches!(commands.recv().await, Some(Command::Interrupt(id)) if id == "first"));
    assert!(commands.try_recv().is_err());
    session.observe(&Event {
        id: "first".into(),
        msg: EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id: Some("first".into()),
            reason: TurnAbortReason::Interrupted,
        }),
    });
    assert_eq!(capacity.available_permits(), 1);
    assert_eq!(
        session.turn("first").unwrap().status,
        TurnStatus::Interrupted
    );
    assert!(session.interrupt("first".into()).is_err());
}

#[test]
fn replay_is_bounded_reports_gaps_and_does_not_expose_oversized_events() {
    let (session, _commands) = ready_session();
    for _ in 0..MAX_EVENTS + 3 {
        session.observe(&complete("turn"));
    }
    let replay = session.events(0).unwrap();
    assert_eq!(replay.events.len(), MAX_EVENTS);
    assert!(replay.gap);
    assert_eq!(replay.next_cursor, (MAX_EVENTS + 3) as u64);
    assert!(!session.events(replay.events[0].sequence - 1).unwrap().gap);
    assert!(session.events(replay.next_cursor + 1).is_err());
    let mut oversized = complete("turn");
    if let EventMsg::TurnComplete(event) = &mut oversized.msg {
        event.last_agent_message = Some("x".repeat(MAX_EVENT_BYTES + 1));
    }
    session.observe(&oversized);
    let last = session.events(replay.next_cursor).unwrap();
    assert_eq!(last.events[0].event["msg"]["type"], "http_event_omitted");
    assert!(session.data.lock().unwrap().replay_bytes <= MAX_REPLAY_BYTES);
}

#[test]
fn retained_records_and_turn_tombstones_are_bounded() {
    let manager = SessionManager::new(1, Duration::from_secs(60));
    let (session, _commands) = manager.reserve(None).unwrap();
    assert!(manager.reserve(None).is_err());
    assert!(manager.forget(&session.id).is_err());
    session.data.lock().unwrap().status = SessionStatus::Closed;
    manager.forget(&session.id).unwrap();
    assert!(manager.reserve(None).is_ok());
    assert!(manager.get(&session.id).is_err());
    assert_ne!(
        manager.daemon_id,
        SessionManager::new(1, Duration::from_secs(60)).daemon_id
    );
}

#[test]
fn result_truncation_keeps_utf8_boundaries() {
    let (text, truncated) = bounded_text("ééé", 5);
    assert_eq!(text, "éé");
    assert!(truncated);
}

#[tokio::test]
async fn foreground_completion_waits_for_kernel_background_quiescence() {
    let (session, _commands) = ready_session();
    let capacity = Arc::new(Semaphore::new(1));
    submit_turn(&session, "first", "hello", &capacity);
    session.observe_with_activity(&complete("first"), false);
    assert_eq!(
        session.turn("first").unwrap().status,
        TurnStatus::WaitingBackground
    );
    assert_eq!(capacity.available_permits(), 0);
    assert!(
        session
            .submit(
                "next".into(),
                request("next"),
                &capacity,
                Duration::from_secs(60)
            )
            .is_err()
    );
    assert!(session.steer("first".into(), "late".into()).await.is_err());
    session.data.lock().unwrap().maybe_finish(true);
    assert_eq!(capacity.available_permits(), 1);
    assert_eq!(session.turn("first").unwrap().status, TurnStatus::Completed);
}

#[tokio::test]
async fn errors_do_not_release_capacity_without_a_terminal_event() {
    let (session, _commands) = ready_session();
    let capacity = Arc::new(Semaphore::new(1));
    submit_turn(&session, "first", "hello", &capacity);
    session.observe(&Event {
        id: "first".into(),
        msg: EventMsg::Error(chaos_ipc::protocol::ErrorEvent {
            message: "test error".into(),
            chaos_error_info: None,
        }),
    });
    assert_eq!(capacity.available_permits(), 0);
    session.observe(&complete("first"));
    assert_eq!(capacity.available_permits(), 1);
    assert_eq!(session.turn("first").unwrap().status, TurnStatus::Failed);
}

#[tokio::test]
async fn turn_history_cap_never_discards_deduplication_evidence() {
    let (session, mut commands) = ready_session();
    let capacity = Arc::new(Semaphore::new(1));
    for index in 0..MAX_TURNS {
        let id = index.to_string();
        session
            .submit(
                id.clone(),
                request("hello"),
                &capacity,
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(matches!(commands.try_recv().unwrap(), Command::Turn(_)));
        session.observe(&complete(&id));
    }
    assert_eq!(
        session
            .submit(
                "next".into(),
                request("hello"),
                &capacity,
                Duration::from_secs(60)
            )
            .unwrap_err()
            .code,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        session
            .submit(
                "0".into(),
                request("hello"),
                &capacity,
                Duration::from_secs(60)
            )
            .unwrap()
            .status,
        TurnStatus::Completed
    );
}

#[test]
fn resume_cannot_create_two_consumers_for_the_same_process() {
    let manager = SessionManager::new(2, Duration::from_secs(60));
    let id = ProcessId::new();
    let (first, _commands) = manager.reserve(Some(id)).unwrap();
    assert!(matches!(manager.reserve(Some(id)), Err(error) if error.code == StatusCode::CONFLICT));
    first.data.lock().unwrap().status = SessionStatus::Closed;
    assert!(manager.reserve(Some(id)).is_ok());
}

#[derive(Debug)]
enum Operation {
    Execute(String, TurnRequest),
    Steer(String, String),
    Interrupt,
    Shutdown,
}

struct FakeBackend {
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<Event>>,
    operations: mpsc::UnboundedSender<Operation>,
    stalled_shutdown: bool,
}

impl Backend for FakeBackend {
    async fn next_event(&self) -> Result<Event, ()> {
        self.events.lock().await.recv().await.ok_or(())
    }
    async fn execute(&self, id: String, request: TurnRequest) -> Result<(), ()> {
        self.operations
            .send(Operation::Execute(id, request))
            .unwrap();
        Ok(())
    }
    async fn steer(&self, id: String, text: String) -> Result<(), ApiError> {
        self.operations.send(Operation::Steer(id, text)).unwrap();
        Ok(())
    }
    async fn interrupt(&self) -> Result<(), ()> {
        self.operations.send(Operation::Interrupt).unwrap();
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), ()> {
        self.operations.send(Operation::Shutdown).unwrap();
        if self.stalled_shutdown {
            std::future::pending().await
        } else {
            Ok(())
        }
    }
}

struct SupervisorFixture {
    events: mpsc::UnboundedSender<Event>,
    operations: mpsc::UnboundedReceiver<Operation>,
    task: tokio::task::JoinHandle<()>,
}

fn start_supervisor(
    session: &Arc<Session>,
    commands: mpsc::Receiver<Command>,
    stalled_shutdown: bool,
    ttl: Duration,
) -> SupervisorFixture {
    let (events, event_rx) = mpsc::unbounded_channel();
    let (operations, operation_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(supervise(
        session.clone(),
        commands,
        FakeBackend {
            events: tokio::sync::Mutex::new(event_rx),
            operations,
            stalled_shutdown,
        },
        Instant::now() + ttl,
    ));
    SupervisorFixture {
        events,
        operations: operation_rx,
        task,
    }
}

async fn wait_until(session: &Session, predicate: impl Fn(&SessionData) -> bool) {
    let mut changed = session.changed.subscribe();
    loop {
        if predicate(&session.data.lock().unwrap()) {
            break;
        }
        changed.changed().await.unwrap();
    }
}

#[tokio::test]
async fn supervisor_keeps_one_backend_across_turns_and_steers_without_stopping_it() {
    let (session, commands) = ready_session();
    let capacity = Arc::new(Semaphore::new(1));
    let mut supervisor = start_supervisor(&session, commands, false, Duration::from_secs(3600));
    submit_turn(&session, "first", "hello", &capacity);
    assert!(
        matches!(supervisor.operations.recv().await, Some(Operation::Execute(id, request)) if id == "first" && request.request == "hello")
    );
    // A streaming HTTP subscriber observes a separate replay buffer, never
    // the kernel queue. Dropping it must not stop this supervisor.
    use rama::http::body::util::BodyExt;
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let mut stream =
        super::stream::response(session.clone(), "test-daemon".into(), 0, shutdown_rx).into_body();
    assert!(stream.frame().await.unwrap().is_ok());
    drop(stream);
    session
        .steer("first".into(), "focus on tests".into())
        .await
        .unwrap();
    assert!(
        matches!(supervisor.operations.recv().await, Some(Operation::Steer(id, text)) if id == "first" && text == "focus on tests")
    );
    supervisor.events.send(complete("first")).unwrap();
    wait_until(&session, |data| data.status == SessionStatus::Idle).await;
    submit_turn(&session, "second", "continue", &capacity);
    assert!(
        matches!(supervisor.operations.recv().await, Some(Operation::Execute(id, _)) if id == "second")
    );
    session.interrupt("second".into()).unwrap();
    assert!(matches!(
        supervisor.operations.recv().await,
        Some(Operation::Interrupt)
    ));
    assert_eq!(capacity.available_permits(), 0);
    session.close().unwrap();
    assert!(matches!(
        supervisor.operations.recv().await,
        Some(Operation::Shutdown)
    ));
    supervisor.task.await.unwrap();
    assert_eq!(session.snapshot().status, SessionStatus::Closed);
    assert_eq!(capacity.available_permits(), 1);
}

#[tokio::test]
async fn cancellation_before_submission_skips_execution_and_cannot_interrupt_the_next_turn() {
    let (session, commands) = ready_session();
    let capacity = Arc::new(Semaphore::new(1));
    submit_turn(&session, "first", "hello", &capacity);
    session.interrupt("first".into()).unwrap();
    let mut supervisor = start_supervisor(&session, commands, false, Duration::from_secs(3600));
    wait_until(&session, |data| data.status == SessionStatus::Idle).await;
    assert_eq!(
        session.turn("first").unwrap().status,
        TurnStatus::Interrupted
    );
    submit_turn(&session, "second", "continue", &capacity);
    // In particular, no Execute(first) or Interrupt is allowed here.
    assert!(
        matches!(supervisor.operations.recv().await, Some(Operation::Execute(id, _)) if id == "second")
    );
    session.close().unwrap();
    assert!(matches!(
        supervisor.operations.recv().await,
        Some(Operation::Shutdown)
    ));
    supervisor.task.await.unwrap();
}

#[tokio::test]
async fn supervisor_owner_loss_marks_unknown_without_releasing_capacity() {
    let (session, _commands) = ready_session();
    let capacity = Arc::new(Semaphore::new(1));
    submit_turn(&session, "first", "hello", &capacity);
    drop(runtime::SupervisorGuard(session.clone()));
    assert_eq!(session.snapshot().status, SessionStatus::Unknown);
    assert_eq!(session.turn("first").unwrap().status, TurnStatus::Unknown);
    assert_eq!(capacity.available_permits(), 0);
}

#[tokio::test(start_paused = true)]
async fn timeout_with_unconfirmed_shutdown_keeps_reservation_and_does_not_replay() {
    let (session, commands) = ready_session();
    let capacity = Arc::new(Semaphore::new(1));
    let mut supervisor = start_supervisor(&session, commands, true, Duration::from_secs(3600));
    session
        .submit(
            "first".into(),
            request("hello"),
            &capacity,
            Duration::from_secs(1),
        )
        .unwrap();
    assert!(matches!(
        supervisor.operations.recv().await,
        Some(Operation::Execute(_, _))
    ));
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        supervisor.operations.recv().await,
        Some(Operation::Shutdown)
    ));
    wait_until(&session, |data| data.status == SessionStatus::Unknown).await;
    assert_eq!(capacity.available_permits(), 0);
    assert_eq!(session.turn("first").unwrap().status, TurnStatus::Unknown);
    assert!(
        session
            .submit(
                "next".into(),
                request("hello"),
                &capacity,
                Duration::from_secs(1)
            )
            .is_err()
    );
    let duplicate = session
        .submit(
            "first".into(),
            request("hello"),
            &capacity,
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(duplicate.status, TurnStatus::Unknown);
    assert!(supervisor.operations.try_recv().is_err());
    supervisor.task.abort();
    let _ = supervisor.task.await;
}

#[tokio::test(start_paused = true)]
async fn idle_session_ttl_closes_backend() {
    let (session, commands) = ready_session();
    let mut supervisor = start_supervisor(&session, commands, false, Duration::from_secs(10));
    assert!(matches!(
        supervisor.operations.recv().await,
        Some(Operation::Shutdown)
    ));
    supervisor.task.await.unwrap();
    assert_eq!(session.snapshot().status, SessionStatus::Closed);
}
