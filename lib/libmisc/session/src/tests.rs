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
