use super::*;
use std::future::pending;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio_test::assert_pending;
use tokio_test::assert_ready;

struct FakeChild(Arc<AtomicUsize>);

impl ChildTerminator for FakeChild {
    fn kill(&mut self) -> io::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The receiver closes only when the task is dropped, including when it is
/// aborted before its first poll. No subprocess or scheduling delay is needed.
fn pending_task() -> (JoinHandle<()>, oneshot::Receiver<()>) {
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        pending::<()>().await;
        drop(tx);
    });
    (task, rx)
}

struct TestProcess {
    handle: ProcessHandle,
    kills: Arc<AtomicUsize>,
    helpers: Vec<oneshot::Receiver<()>>,
    stdin: mpsc::Receiver<Vec<u8>>,
}

impl TestProcess {
    fn new() -> Self {
        let (writer_tx, stdin) = mpsc::channel(1);
        let kills = Arc::new(AtomicUsize::new(0));
        let (reader, reader_done) = pending_task();
        let (stdout, stdout_done) = pending_task();
        let (stderr, stderr_done) = pending_task();
        let (writer, writer_done) = pending_task();
        let (wait, wait_done) = pending_task();
        let handle = ProcessHandle::new(
            writer_tx,
            Box::new(FakeChild(Arc::clone(&kills))),
            reader,
            vec![stdout.abort_handle(), stderr.abort_handle()],
            writer,
            wait,
            Arc::new(AtomicBool::new(false)),
            Arc::new(StdMutex::new(None)),
            None,
        );
        // These tasks are detached just like pipe readers whose join handles
        // were owned by an aborted aggregate reader task.
        drop(stdout);
        drop(stderr);
        Self {
            handle,
            kills,
            helpers: vec![
                reader_done,
                stdout_done,
                stderr_done,
                writer_done,
                wait_done,
            ],
            stdin,
        }
    }
}

async fn assert_helpers_stopped(helpers: Vec<oneshot::Receiver<()>>) {
    for done in helpers {
        assert!(done.await.is_err(), "helper must drop its sender on abort");
    }
}

#[tokio::test]
async fn terminate_kills_once_and_aborts_all_helpers() {
    let process = TestProcess::new();

    process.handle.terminate();
    process.handle.terminate();
    assert_eq!(process.kills.load(Ordering::SeqCst), 1);
    assert_helpers_stopped(process.helpers).await;
}

#[tokio::test]
async fn drop_kills_child_and_aborts_all_helpers() {
    let process = TestProcess::new();

    drop(process.handle);
    assert_eq!(process.kills.load(Ordering::SeqCst), 1);
    assert_helpers_stopped(process.helpers).await;
}

#[tokio::test]
async fn request_terminate_preserves_helpers_for_output_draining() {
    let mut process = TestProcess::new();

    process.handle.request_terminate();
    process.handle.request_terminate();
    assert_eq!(process.kills.load(Ordering::SeqCst), 1);
    for done in &mut process.helpers {
        assert_eq!(done.try_recv(), Err(oneshot::error::TryRecvError::Empty));
    }

    process.handle.terminate();
    assert_eq!(process.kills.load(Ordering::SeqCst), 1);
    assert_helpers_stopped(process.helpers).await;
}

#[tokio::test]
async fn close_stdin_disconnects_the_channel_without_killing_the_child() {
    let mut process = TestProcess::new();
    process
        .handle
        .writer_sender()
        .send(b"input".to_vec())
        .await
        .expect("send input");
    assert_eq!(process.stdin.try_recv().expect("input"), b"input");

    process.handle.close_stdin();
    assert!(process.handle.writer_sender().is_closed());
    assert_eq!(
        process.stdin.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    );
    assert_eq!(process.kills.load(Ordering::SeqCst), 0);
    drop(process.handle);
    assert_helpers_stopped(process.helpers).await;
}

#[test]
fn combined_output_drains_stderr_while_stdout_is_idle() {
    let (stdout, stdout_rx) = mpsc::channel(1);
    let (stderr, stderr_rx) = mpsc::channel(1);
    let (combined_tx, mut combined) = broadcast::channel(8);
    let mut forwarding = tokio_test::task::spawn(forward_output(stdout_rx, stderr_rx, combined_tx));
    assert_pending!(forwarding.poll());

    stderr.try_send(b"stderr".to_vec()).expect("stderr");
    assert_pending!(forwarding.poll());
    assert_eq!(combined.try_recv().expect("stderr output"), b"stderr");
    stdout.try_send(b"stdout".to_vec()).expect("stdout");
    assert_pending!(forwarding.poll());
    assert_eq!(combined.try_recv().expect("stdout output"), b"stdout");

    drop(stdout);
    drop(stderr);
    assert_ready!(forwarding.poll());
    assert_eq!(
        combined.try_recv(),
        Err(broadcast::error::TryRecvError::Closed)
    );
}

#[test]
fn combined_output_stays_open_until_both_streams_close() {
    for close_stdout_first in [true, false] {
        let (stdout, stdout_rx) = mpsc::channel(1);
        let (stderr, stderr_rx) = mpsc::channel(1);
        let (combined_tx, mut combined) = broadcast::channel(8);
        let mut forwarding =
            tokio_test::task::spawn(forward_output(stdout_rx, stderr_rx, combined_tx));
        let remaining = if close_stdout_first {
            drop(stdout);
            stderr
        } else {
            drop(stderr);
            stdout
        };

        // Process the first EOF before sending any final output. Merely
        // spawning the bridge would leave this ordering up to the scheduler.
        assert_pending!(forwarding.poll());
        assert_eq!(
            combined.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        );
        remaining
            .try_send(b"last chunk".to_vec())
            .expect("last chunk");
        drop(remaining);
        assert_ready!(forwarding.poll());
        assert_eq!(combined.try_recv().expect("final output"), b"last chunk");
        assert_eq!(
            combined.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        );
    }
}
