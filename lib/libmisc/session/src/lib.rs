//! `chaos-session` — userland harness for a kernel conversation session.
//!
//! Wraps the Submission Queue / Event Queue pattern exposed by
//! [`chaos_ipc::protocol`] into a single [`ClientSession`] type that every
//! frontend — TUI, GUI, headless — can use identically.
//!
//! This is the shared libc-style boundary between userland and the kernel's
//! conversation loop. If a binary wants to talk to a `chaos-kern` process, it
//! goes through here and nothing else. When the SQ/EQ surface changes, this
//! is the one file that has to track it.
//!
//! # Overview
//!
//! A [`ClientSession`] is a pair of channels:
//!
//! * `op_tx` — the Submission Queue. Send [`Op`] values to ask the kernel to
//!   do work (user input, interrupts, config reloads, ...).
//! * `event_rx` — the Event Queue. Receive [`Event`] values describing what
//!   the kernel did in response (message deltas, tool calls, errors,
//!   shutdowns, ...).
//!
//! The session owns a background task that pumps both queues against an
//! underlying [`Process`]. You own the channels; the session ends when
//! `event_rx` yields [`EventMsg::ShutdownComplete`] or closes.
//!
//! # Task lifecycle
//!
//! Internally the session runs two background tasks: one drains the
//! submission queue into [`Process::submit`], the other pumps
//! [`Process::next_event`] into `event_rx`. They share a private
//! [`CancellationToken`]: the forward-events task holds a
//! [`tokio_util::sync::DropGuard`] so its termination — for any reason —
//! automatically cancels the op-drain task. This closes the footgun where
//! a caller that keeps `op_tx` alive past [`EventMsg::ShutdownComplete`]
//! would otherwise strand the op-drain task forever.
//!
//! # Termination semantics
//!
//! The session is considered terminated — and op submission stops being
//! honoured — when **any** of the following occurs:
//!
//! * the kernel emits [`EventMsg::ShutdownComplete`];
//! * [`Process::next_event`] returns an error (kernel died / crashed);
//! * the caller drops `event_rx` (observed via send-error from the
//!   forward-events task);
//! * the forward-events task panics.
//!
//! In all cases the shared [`CancellationToken`] fires, the op-drain
//! select-loop exits on its next iteration, and any further `op_tx.send(..)`
//! calls succeed against a dead receiver — the ops are silently discarded.
//! This matters especially for the third case: **dropping `event_rx` while
//! still holding `op_tx` disables op submission**. Keep both handles alive
//! if you still expect the kernel to act on your submissions.
//!
//! # Entry points
//!
//! * [`ClientSession::spawn`] — cold start a fresh kernel process from a
//!   [`Config`].
//! * [`ClientSession::attach`] — attach to an existing [`Arc<Process>`],
//!   replaying a captured [`SessionConfiguredEvent`] first (used after fork,
//!   resume, or clone).
//! * [`spawn_op_forwarder`] — submit-only loop for callers that already own
//!   the event stream (or don't care about it).
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use chaos_ipc::protocol::{EventMsg, Op};
//! use chaos_kern::{ProcessTable, config::Config};
//! use chaos_session::ClientSession;
//!
//! # async fn demo(config: Config, table: Arc<ProcessTable>) {
//! let mut session = ClientSession::spawn(
//!     config,
//!     table,
//!     Some("my-frontend".to_string()),
//! );
//!
//! // First event is always SessionConfigured (or Error on boot failure).
//! while let Some(event) = session.event_rx.recv().await {
//!     match event.msg {
//!         EventMsg::SessionConfigured(_) => {
//!             session.op_tx.send(Op::Interrupt).ok();
//!         }
//!         EventMsg::ShutdownComplete => break,
//!         _ => {}
//!     }
//! }
//! # }
//! ```

use std::future::Future;
use std::sync::Arc;

use chaos_ipc::protocol::{Event, EventMsg, Op, SessionConfiguredEvent};
use chaos_kern::config::Config;
use chaos_kern::{Process, ProcessTable};
use tokio::sync::mpsc::error::SendError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::{CancellationToken, DropGuard};

/// Minimal kernel-process surface the session runtime needs.
///
/// Exists so [`drain_ops`] and [`forward_events`] can be unit-tested against
/// a fake backend that doesn't require spinning up a real
/// [`chaos_kern::Process`]. The production path uses the blanket impl below
/// over the concrete [`Process`]; tests substitute a channel-backed fake.
///
/// The trait deliberately throws away the specific error types used by the
/// kernel — the session loops only care about success vs. failure.
trait KernelProc: Send + Sync + 'static {
    /// Submit an op to the underlying process.
    fn drive_submit(&self, op: Op) -> impl Future<Output = Result<(), String>> + Send + '_;
    /// Block on the next event from the underlying process.
    fn drive_next_event(&self) -> impl Future<Output = Result<Event, String>> + Send + '_;
}

impl KernelProc for Process {
    async fn drive_submit(&self, op: Op) -> Result<(), String> {
        Process::submit(self, op)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn drive_next_event(&self) -> Result<Event, String> {
        Process::next_event(self).await.map_err(|e| e.to_string())
    }
}

/// A live client session talking to a kernel process.
///
/// Hold this struct for as long as you want to talk to the kernel. Submit
/// work via [`op_tx`](Self::op_tx). Consume results via
/// [`event_rx`](Self::event_rx). The first event is always
/// [`EventMsg::SessionConfigured`] on success or [`EventMsg::Error`] if the
/// kernel failed to boot.
///
/// Dropping the `ClientSession` drops the channels; the background pump task
/// will notice the receiver is gone and exit on its next iteration.
pub struct ClientSession {
    /// Submission queue: send [`Op`] values to drive the kernel.
    pub op_tx: UnboundedSender<Op>,
    /// Event queue: receive [`Event`] values describing kernel activity.
    pub event_rx: UnboundedReceiver<Event>,
}

impl ClientSession {
    /// Cold start: spawn a new kernel process for `config` and begin
    /// forwarding events from it.
    ///
    /// Does **not** block on the kernel boot. Returns immediately with empty
    /// channels; the boot runs on a background task. If [`ProcessTable::start_process`]
    /// fails, the first event delivered on `event_rx` will be
    /// [`EventMsg::Error`] and the pump exits.
    ///
    /// `client_name` is forwarded to
    /// [`Process::set_app_server_client_name`] so the kernel can identify who
    /// is attached. Pass `None` to leave it unset.
    pub fn spawn(
        config: Config,
        process_table: Arc<ProcessTable>,
        client_name: Option<String>,
    ) -> Self {
        let (op_tx, op_rx) = unbounded_channel::<Op>();
        let (event_tx, event_rx) = unbounded_channel::<Event>();

        let cancel = CancellationToken::new();

        tokio::spawn(async move {
            let new_process = match process_table.start_process(config).await {
                Ok(p) => p,
                Err(err) => {
                    tracing::error!("failed to initialize chaos: {err}");
                    let _ = event_tx.send(Event {
                        id: String::new(),
                        msg: EventMsg::Error(err.to_error_event(None)),
                    });
                    return;
                }
            };

            let (_, thread, session_configured) = new_process.into_parts();
            set_client_name(thread.as_ref(), client_name).await;

            if event_tx
                .send(Event {
                    id: String::new(),
                    msg: EventMsg::SessionConfigured(session_configured),
                })
                .is_err()
            {
                return;
            }

            spawn_op_loop(thread.clone(), op_rx, cancel.clone());
            forward_events(thread, event_tx, cancel).await;
        });

        Self { op_tx, event_rx }
    }

    /// Warm start: attach to an existing [`Process`] and replay a captured
    /// [`SessionConfiguredEvent`] as the first event.
    ///
    /// Used by flows like fork, resume, and collab-clone where the kernel
    /// process already exists and its configuration event was captured
    /// elsewhere.
    pub fn attach(
        process: Arc<Process>,
        session_configured: SessionConfiguredEvent,
        client_name: Option<String>,
    ) -> Self {
        let (op_tx, op_rx) = unbounded_channel::<Op>();
        let (event_tx, event_rx) = unbounded_channel::<Event>();

        let cancel = CancellationToken::new();

        tokio::spawn(async move {
            set_client_name(process.as_ref(), client_name).await;

            if event_tx
                .send(Event {
                    id: String::new(),
                    msg: EventMsg::SessionConfigured(session_configured),
                })
                .is_err()
            {
                return;
            }

            spawn_op_loop(process.clone(), op_rx, cancel.clone());
            forward_events(process, event_tx, cancel).await;
        });

        Self { op_tx, event_rx }
    }
}

/// A submit-only op handle paired with its drain task's shutdown guard.
///
/// Returned by [`spawn_op_forwarder`]. Dropping this value cancels the
/// background drain task and releases its held [`Arc<Process>`] even if
/// other clones of the underlying [`UnboundedSender`] are still alive —
/// which fixes the leak class where a forked process whose event pump
/// lives elsewhere could otherwise strand the drain task forever on a dead
/// kernel.
///
/// # API surface
///
/// `OpForwarder` deliberately exposes only [`send`] and [`clone_sender`]
/// rather than implementing `Deref<Target = UnboundedSender<Op>>`. The
/// reason: an autoderef `Deref` impl would also expose `Clone::clone` from
/// the inner sender, so `forwarder.clone()` would silently produce a bare
/// `UnboundedSender<Op>` instead of another `OpForwarder` — a footgun in a
/// type whose entire purpose is coupling the sender to the drain-task
/// lifetime. If you need a sender that outlives the forwarder, ask for it
/// explicitly with [`clone_sender`].
///
/// [`send`]: Self::send
/// [`clone_sender`]: Self::clone_sender
#[must_use = "dropping the OpForwarder cancels its drain task — bind it to a field, not _"]
pub struct OpForwarder {
    op_tx: UnboundedSender<Op>,
    /// Drop guard on the drain task's cancellation token. Cancels on drop,
    /// `forward_events`-style: any exit path releases the drain task's
    /// `Arc<Process>`.
    _shutdown: DropGuard,
}

impl OpForwarder {
    /// Wrap a raw [`UnboundedSender<Op>`] in an [`OpForwarder`] shell that
    /// does **not** own a drain task.
    ///
    /// Used by call sites that already get their sender from a full
    /// [`ClientSession`] — whose event pump carries its own DropGuard
    /// cascade — but want to expose a single [`OpForwarder`]-typed handle
    /// to their UI layer for consistency. The returned forwarder is a
    /// no-op on drop.
    pub fn from_sender(op_tx: UnboundedSender<Op>) -> Self {
        // A cancel token with no listeners. Fires on drop but cancels
        // nothing, because nothing is listening.
        let orphan = CancellationToken::new().drop_guard();
        Self {
            op_tx,
            _shutdown: orphan,
        }
    }

    /// Submit an [`Op`] to the underlying drain task.
    ///
    /// Returns the same [`SendError`] as the inner [`UnboundedSender`] when
    /// the receiver is gone (i.e. the drain task has exited).
    //
    // Allow the large-Err lint: `SendError<Op>` mirrors `UnboundedSender::send`'s
    // own return type, so we deliberately preserve the same shape rather than
    // boxing here and forcing every callsite to deal with two indirection layers.
    #[allow(clippy::result_large_err)]
    pub fn send(&self, op: Op) -> Result<(), SendError<Op>> {
        self.op_tx.send(op)
    }

    /// Clone an independent [`UnboundedSender<Op>`] that survives drop of
    /// the forwarder. The clone can still submit ops until the underlying
    /// drain task exits (at which point sends start erroring).
    pub fn clone_sender(&self) -> UnboundedSender<Op> {
        self.op_tx.clone()
    }
}

/// Spawn a submit-only loop for an existing [`Process`] without subscribing
/// to its event stream.
///
/// Useful when the caller already owns the event-receiving side (e.g. it
/// forked off a session whose events are being drained elsewhere) and only
/// needs a channel to push [`Op`] values.
///
/// # Lifecycle
///
/// Unlike [`ClientSession`], this forwarder has no paired event pump to
/// coordinate with, so there is no automatic signal telling the drain task
/// to give up when the underlying `Process` dies. To close the gap this
/// function returns an [`OpForwarder`] whose `Drop` impl cancels the drain
/// task via a [`DropGuard`] on a private [`CancellationToken`]. Callers
/// **must** hold the [`OpForwarder`] for as long as they want to submit
/// ops, and drop it to release the drain task and the held
/// [`Arc<Process>`]. Raw clones via [`OpForwarder::clone_sender`] do *not*
/// extend the drain task's lifetime.
pub fn spawn_op_forwarder(process: Arc<Process>, client_name: Option<String>) -> OpForwarder {
    let (op_tx, op_rx) = unbounded_channel::<Op>();

    // The forwarder owns the drop guard; the drain task holds a clone of
    // the same token. When the guard is dropped the token fires and
    // drain_ops exits on its next select iteration (the biased cancel arm
    // wins over op_rx.recv).
    let cancel = CancellationToken::new();
    let drain_token = cancel.clone();

    tokio::spawn(async move {
        set_client_name(process.as_ref(), client_name).await;
        drain_ops(process, op_rx, drain_token).await;
    });

    OpForwarder {
        op_tx,
        _shutdown: cancel.drop_guard(),
    }
}

async fn set_client_name(process: &Process, client_name: Option<String>) {
    if let Err(err) = process.set_app_server_client_name(client_name).await {
        tracing::error!("failed to set app server client name: {err}");
    }
}

fn spawn_op_loop<P: KernelProc>(
    process: Arc<P>,
    op_rx: UnboundedReceiver<Op>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        drain_ops(process, op_rx, cancel).await;
    });
}

async fn drain_ops<P: KernelProc>(
    process: Arc<P>,
    mut op_rx: UnboundedReceiver<Op>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                // Event pump ended (shutdown, kernel error, dropped event_rx).
                // Exit so we don't strand ourselves on a dead process.
                break;
            }
            maybe_op = op_rx.recv() => {
                let Some(op) = maybe_op else {
                    // All op_tx senders dropped — session is done from the
                    // caller's side. Exit cleanly.
                    break;
                };
                if let Err(e) = process.drive_submit(op).await {
                    tracing::error!("failed to submit op: {e}");
                }
            }
        }
    }
}

async fn forward_events<P: KernelProc>(
    process: Arc<P>,
    event_tx: UnboundedSender<Event>,
    cancel: CancellationToken,
) {
    // Convert the token into a drop guard so op-drain is cancelled no matter
    // how we leave this function — clean exit, error, or panic.
    let _guard = cancel.drop_guard();
    while let Ok(event) = process.drive_next_event().await {
        let is_shutdown = matches!(event.msg, EventMsg::ShutdownComplete);
        if event_tx.send(event).is_err() {
            break;
        }
        if is_shutdown {
            // ShutdownComplete is terminal for a process; drop our receiver
            // task so the Arc<Process> can be released and resources cleaned
            // up.
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    //! Regression coverage for the DropGuard-based cascading shutdown added
    //! in #17. These tests use a channel-backed [`FakeProc`] so every exit
    //! path of [`forward_events`] — ShutdownComplete, dropped event_rx, and
    //! `next_event` errors — can be exercised without a real kernel.
    //!
    //! The invariant they all protect is the footgun called out in the
    //! module docs: "dropping `event_rx` while still holding `op_tx`
    //! disables op submission." The DropGuard on the shared
    //! [`CancellationToken`] is what converts that from "hangs forever" into
    //! "drain_ops exits cleanly on its next iteration."
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use chaos_ipc::protocol::{Event, EventMsg, Op, WarningEvent};
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::Notify;
    use tokio::sync::mpsc::{self, unbounded_channel};
    use tokio::time::timeout;
    use tokio_util::sync::CancellationToken;

    use super::{KernelProc, OpForwarder, drain_ops, forward_events};

    /// Asserting future completion when the production code is supposed to
    /// exit. Generous enough to survive a loaded CI box but tight enough to
    /// catch a regression that actually hangs.
    const EXIT_TIMEOUT: Duration = Duration::from_secs(2);

    /// Kernel-process fake driven by channels.
    ///
    /// * `submitted` records every op that made it through `drive_submit`
    ///   so callers can assert on delivery order / count.
    /// * `events` pops one scripted [`Result<Event, String>`] per
    ///   `drive_next_event` call, yielding `Err("closed")` once the script
    ///   is exhausted — this models a dead kernel for tests that want to
    ///   drive `forward_events` off the end of the while-let.
    struct FakeProc {
        submitted: StdMutex<Vec<Op>>,
        submitted_notify: Notify,
        events: AsyncMutex<mpsc::UnboundedReceiver<Result<Event, String>>>,
    }

    impl FakeProc {
        fn new(events_rx: mpsc::UnboundedReceiver<Result<Event, String>>) -> Self {
            Self {
                submitted: StdMutex::new(Vec::new()),
                submitted_notify: Notify::new(),
                events: AsyncMutex::new(events_rx),
            }
        }

        fn submitted(&self) -> Vec<Op> {
            self.submitted.lock().expect("submitted lock").clone()
        }

        /// Wait until `drive_submit` has been called at least `target` times,
        /// so tests can barrier on "drain_ops actually processed my op"
        /// before tripping the shutdown cascade.
        ///
        /// Uses the canonical [`Notify::notified`] + `enable()`-before-check
        /// pattern: we must register as a waiter *before* inspecting the
        /// shared state so a `notify_waiters()` racing against our check
        /// can't fire into an empty waiter set and get lost. Without
        /// `enable()` the waker registration happens on first poll of
        /// `notified.await`, which is after the length check — that window
        /// would be a classic lost-wakeup bug that only shows up as
        /// intermittent CI hangs timing out against `EXIT_TIMEOUT`.
        async fn wait_for_submit_count(&self, target: usize) {
            loop {
                let notified = self.submitted_notify.notified();
                tokio::pin!(notified);
                // Register as a waiter now so a subsequent notify_waiters()
                // cannot fire before we've subscribed.
                notified.as_mut().enable();
                if self.submitted.lock().expect("submitted lock").len() >= target {
                    return;
                }
                notified.await;
            }
        }
    }

    impl KernelProc for FakeProc {
        async fn drive_submit(&self, op: Op) -> Result<(), String> {
            self.submitted.lock().expect("submitted lock").push(op);
            self.submitted_notify.notify_waiters();
            Ok(())
        }

        async fn drive_next_event(&self) -> Result<Event, String> {
            let mut rx = self.events.lock().await;
            match rx.recv().await {
                Some(result) => result,
                None => Err("fake kernel event channel closed".to_string()),
            }
        }
    }

    fn ev(msg: EventMsg) -> Event {
        Event {
            id: String::new(),
            msg,
        }
    }

    /// drain_ops in isolation:
    ///
    /// * forwards every op to the process while op_tx is alive;
    /// * exits cleanly when the last op_tx sender is dropped;
    /// * exits cleanly when the cancel token fires mid-wait.
    #[tokio::test]
    async fn drain_ops_forwards_then_exits_on_sender_drop_and_on_cancel() {
        // Case 1: natural shutdown — drop op_tx after submitting work.
        let (_events_tx, events_rx) = mpsc::unbounded_channel();
        let fake = std::sync::Arc::new(FakeProc::new(events_rx));
        let (op_tx, op_rx) = unbounded_channel::<Op>();
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(drain_ops(fake.clone(), op_rx, cancel.clone()));

        op_tx.send(Op::Interrupt).expect("submit interrupt");
        op_tx.send(Op::Interrupt).expect("submit interrupt 2");
        drop(op_tx);

        timeout(EXIT_TIMEOUT, handle)
            .await
            .expect("drain_ops must exit when op_tx is dropped")
            .expect("drain_ops task panicked");
        assert_eq!(fake.submitted().len(), 2, "both ops must reach process");
        assert!(
            !cancel.is_cancelled(),
            "natural shutdown does not fire the cancel token"
        );

        // Case 2: external cancel fires while drain_ops is parked on op_rx.
        let (_events_tx2, events_rx2) = mpsc::unbounded_channel();
        let fake2 = std::sync::Arc::new(FakeProc::new(events_rx2));
        let (_op_tx2, op_rx2) = unbounded_channel::<Op>();
        let cancel2 = CancellationToken::new();
        let handle2 = tokio::spawn(drain_ops(fake2, op_rx2, cancel2.clone()));

        cancel2.cancel();
        timeout(EXIT_TIMEOUT, handle2)
            .await
            .expect("drain_ops must exit when cancel fires")
            .expect("drain_ops task panicked");
    }

    /// Full forward_events + drain_ops wiring: ShutdownComplete is terminal
    /// for the event pump, the DropGuard fires on exit, and drain_ops
    /// unblocks on its next iteration — closing out the op-submission loop
    /// even though `op_tx` is still held by the test.
    #[tokio::test]
    async fn shutdown_complete_cascades_cancel_to_drain_ops() {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let fake = std::sync::Arc::new(FakeProc::new(events_rx));
        let (op_tx, op_rx) = unbounded_channel::<Op>();
        let (client_event_tx, mut client_event_rx) = unbounded_channel::<Event>();
        let cancel = CancellationToken::new();

        let forward = tokio::spawn(forward_events(
            fake.clone(),
            client_event_tx,
            cancel.clone(),
        ));
        let drain = tokio::spawn(drain_ops(fake.clone(), op_rx, cancel.clone()));

        // Submit before shutdown and barrier on the submission reaching the
        // fake so we know drain_ops actually processed it before the cascade
        // fires. Without the barrier the `biased` select in drain_ops lets
        // the cancel branch steal the first iteration.
        op_tx.send(Op::Interrupt).expect("submit interrupt");
        timeout(EXIT_TIMEOUT, fake.wait_for_submit_count(1))
            .await
            .expect("drain_ops must forward the op before shutdown");

        events_tx
            .send(Ok(ev(EventMsg::Warning(WarningEvent {
                message: "test".to_string(),
            }))))
            .unwrap();
        events_tx.send(Ok(ev(EventMsg::ShutdownComplete))).unwrap();

        // Both tasks must exit — forward_events on the terminal event,
        // drain_ops via the DropGuard cancel.
        timeout(EXIT_TIMEOUT, forward)
            .await
            .expect("forward_events must exit on ShutdownComplete")
            .expect("forward_events panicked");
        timeout(EXIT_TIMEOUT, drain)
            .await
            .expect("drain_ops must exit after cascade")
            .expect("drain_ops panicked");
        assert!(cancel.is_cancelled(), "DropGuard must have fired");

        // Both events reached the client.
        let first = client_event_rx.recv().await.expect("first event");
        assert!(matches!(first.msg, EventMsg::Warning(_)));
        let second = client_event_rx.recv().await.expect("shutdown event");
        assert!(matches!(second.msg, EventMsg::ShutdownComplete));

        // op_tx is still alive here — and yet drain_ops is gone. This is
        // the explicit footgun the module docs warn about: submissions after
        // the cascade never reach the kernel. Whether the send errors or is
        // silently queued against a dropped receiver depends on timing; what
        // matters is that the fake sees no additional ops.
        let _ = op_tx.send(Op::Interrupt);
        assert_eq!(
            fake.submitted().len(),
            1,
            "no further ops reach the fake after cascade"
        );
    }

    /// The other two forward_events exit paths must also trip the
    /// DropGuard: (a) the caller drops `event_rx`, so `event_tx.send` errors;
    /// (b) `drive_next_event` returns Err, modelling a dead kernel.
    #[tokio::test]
    async fn forward_events_cascade_on_event_rx_drop_and_on_next_event_error() {
        // (a) event_rx dropped — forward_events breaks on send error.
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let fake = std::sync::Arc::new(FakeProc::new(events_rx));
        let (_op_tx, op_rx) = unbounded_channel::<Op>();
        let (client_event_tx, client_event_rx) = unbounded_channel::<Event>();
        let cancel = CancellationToken::new();

        let forward = tokio::spawn(forward_events(
            fake.clone(),
            client_event_tx,
            cancel.clone(),
        ));
        let drain = tokio::spawn(drain_ops(fake.clone(), op_rx, cancel.clone()));

        // Consumer leaves before any event arrives.
        drop(client_event_rx);
        events_tx
            .send(Ok(ev(EventMsg::Warning(WarningEvent {
                message: "test".to_string(),
            }))))
            .unwrap();

        timeout(EXIT_TIMEOUT, forward)
            .await
            .expect("forward_events must exit when event_rx is dropped")
            .expect("forward_events panicked");
        timeout(EXIT_TIMEOUT, drain)
            .await
            .expect("drain_ops must exit after cascade")
            .expect("drain_ops panicked");
        assert!(cancel.is_cancelled());

        // (b) drive_next_event error — models a crashed kernel.
        let (events_tx2, events_rx2) = mpsc::unbounded_channel();
        let fake2 = std::sync::Arc::new(FakeProc::new(events_rx2));
        let (_op_tx2, op_rx2) = unbounded_channel::<Op>();
        let (client_event_tx2, mut client_event_rx2) = unbounded_channel::<Event>();
        let cancel2 = CancellationToken::new();

        let forward2 = tokio::spawn(forward_events(
            fake2.clone(),
            client_event_tx2,
            cancel2.clone(),
        ));
        let drain2 = tokio::spawn(drain_ops(fake2, op_rx2, cancel2.clone()));

        events_tx2
            .send(Ok(ev(EventMsg::Warning(WarningEvent {
                message: "test".to_string(),
            }))))
            .unwrap();
        events_tx2.send(Err("kernel died".to_string())).unwrap();

        timeout(EXIT_TIMEOUT, forward2)
            .await
            .expect("forward_events must exit on drive_next_event error")
            .expect("forward_events panicked");
        timeout(EXIT_TIMEOUT, drain2)
            .await
            .expect("drain_ops must exit after cascade")
            .expect("drain_ops panicked");
        assert!(cancel2.is_cancelled());

        // The healthy event still reached the client; the error was swallowed.
        let first = client_event_rx2.recv().await.expect("first event");
        assert!(matches!(first.msg, EventMsg::Warning(_)));
        assert!(
            client_event_rx2.try_recv().is_err(),
            "errors from drive_next_event are not forwarded"
        );
    }

    /// Regression for task #18: dropping an [`OpForwarder`] must cancel the
    /// drain task even when external clones of `op_tx` are still alive.
    ///
    /// Without the `DropGuard` the drain task would wait on `op_rx.recv()`
    /// forever (external clone keeps the channel alive) and leak the
    /// `Arc<Process>` it holds — the same leak class that #17 fixed for
    /// `ClientSession`, on the submit-only code path.
    #[tokio::test]
    async fn op_forwarder_drop_cancels_drain_task_despite_external_sender_clone() {
        let (_events_tx, events_rx) = mpsc::unbounded_channel();
        let fake = std::sync::Arc::new(FakeProc::new(events_rx));
        let (op_tx, op_rx) = unbounded_channel::<Op>();
        let cancel = CancellationToken::new();
        let drain_handle = tokio::spawn(drain_ops(fake.clone(), op_rx, cancel.clone()));

        // Build an OpForwarder by hand that mirrors spawn_op_forwarder's
        // internal wiring, then clone the sender BEFORE dropping so the
        // drain task's `op_rx` still has a live upstream after the drop.
        // If drain_ops relied on op_tx-drop as its only termination signal
        // (the pre-#18 behaviour), that external clone would strand it
        // forever — the test times out.
        let forwarder = OpForwarder {
            op_tx: op_tx.clone(),
            _shutdown: cancel.clone().drop_guard(),
        };
        let external_clone = forwarder.clone_sender();
        drop(op_tx); // collapse the stray local sender

        forwarder.send(Op::Interrupt).expect("submit via forwarder");
        timeout(EXIT_TIMEOUT, fake.wait_for_submit_count(1))
            .await
            .expect("drain_ops must process the pre-drop op");

        // Drop the forwarder. `external_clone` is still alive, so the
        // drain task can only exit via the DropGuard cancel — not via
        // op_rx's None branch.
        drop(forwarder);

        timeout(EXIT_TIMEOUT, drain_handle)
            .await
            .expect("drain_ops must exit when OpForwarder is dropped")
            .expect("drain_ops panicked");
        assert!(
            cancel.is_cancelled(),
            "OpForwarder::Drop must fire the cancel token"
        );

        // Post-cascade: no further ops reach the fake even though the
        // external clone is still held and callable.
        let _ = external_clone.send(Op::Interrupt);
        assert_eq!(
            fake.submitted().len(),
            1,
            "no ops reach the fake after forwarder drop"
        );
    }

    /// [`OpForwarder::from_sender`] must be a true no-op on drop: the
    /// caller's sender lives on untouched, because the wrapper never
    /// spawned a drain task in the first place. Regression guard against
    /// accidentally wiring a real cancel into the orphan path.
    #[tokio::test]
    async fn op_forwarder_from_sender_drop_does_not_touch_caller_sender() {
        let (tx, mut rx) = unbounded_channel::<Op>();
        let wrapper = OpForwarder::from_sender(tx.clone());
        wrapper.send(Op::Interrupt).expect("send via wrapper");
        drop(wrapper);
        // The original tx is still alive and reachable.
        tx.send(Op::Interrupt)
            .expect("original sender survives drop");
        assert!(matches!(rx.recv().await, Some(Op::Interrupt)));
        assert!(matches!(rx.recv().await, Some(Op::Interrupt)));
    }
}
