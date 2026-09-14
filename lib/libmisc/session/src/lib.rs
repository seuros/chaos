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
pub mod background;
use std::sync::Arc;

use chaos_ipc::product::OS_NAME;
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
                    tracing::error!("failed to initialize {OS_NAME}: {err}");
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
mod tests;
