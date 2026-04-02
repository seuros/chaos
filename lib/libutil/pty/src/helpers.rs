#![allow(clippy::module_name_repetitions)]

use std::io::ErrorKind;
use std::io::Read;
use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Decomposed exit-tracking state returned by [`ExitTracker::decompose`].
pub(crate) type ExitParts = (
    Arc<AtomicBool>,
    Arc<StdMutex<Option<i32>>>,
    oneshot::Receiver<i32>,
    Box<dyn FnOnce(i32) + Send + 'static>,
);

/// Shared state for tracking child process exit.
///
/// Bundles the `AtomicBool` flag, the `StdMutex<Option<i32>>` exit code,
/// and the oneshot sender so every spawn site doesn't have to repeat the
/// wiring.
pub(crate) struct ExitTracker {
    pub exit_status: Arc<AtomicBool>,
    pub exit_code: Arc<StdMutex<Option<i32>>>,
    pub exit_rx: oneshot::Receiver<i32>,
    exit_tx: Option<oneshot::Sender<i32>>,
}

impl ExitTracker {
    pub fn new() -> Self {
        let (exit_tx, exit_rx) = oneshot::channel::<i32>();
        Self {
            exit_status: Arc::new(AtomicBool::new(false)),
            exit_code: Arc::new(StdMutex::new(None)),
            exit_rx,
            exit_tx: Some(exit_tx),
        }
    }

    /// Split into the parts needed by `ProcessHandle::new` and a callback-
    /// friendly recorder closure, plus the `exit_rx` for `SpawnedProcess`.
    ///
    /// Returns `(exit_status, exit_code, exit_rx, recorder)` where `recorder`
    /// is a `FnOnce(i32)` that stores the code and fires the oneshot.
    pub fn decompose(self) -> ExitParts {
        let wait_exit_status = Arc::clone(&self.exit_status);
        let wait_exit_code = Arc::clone(&self.exit_code);
        let exit_tx = self.exit_tx;
        let recorder = move |code: i32| {
            wait_exit_status.store(true, Ordering::SeqCst);
            if let Ok(mut guard) = wait_exit_code.lock() {
                *guard = Some(code);
            }
            if let Some(tx) = exit_tx {
                let _ = tx.send(code);
            }
        };
        (
            self.exit_status,
            self.exit_code,
            self.exit_rx,
            Box::new(recorder),
        )
    }
}

/// Spawn a blocking read loop on a `std::io::Read` source, forwarding
/// chunks into an mpsc channel.
///
/// Uses an 8 KiB buffer. Retries on `Interrupted`, sleeps briefly on
/// `WouldBlock`, and breaks on EOF or any other error.
pub(crate) fn spawn_blocking_read_loop<R>(
    mut reader: R,
    tx: mpsc::Sender<Vec<u8>>,
) -> JoinHandle<()>
where
    R: Read + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8_192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = tx.blocking_send(buf[..n].to_vec());
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            }
        }
    })
}

/// Spawn an async writer task that drains an mpsc receiver and writes each
/// chunk into a `std::io::Write` sink wrapped in `Arc<tokio::sync::Mutex>`.
pub(crate) fn spawn_blocking_writer<W>(
    writer: Arc<tokio::sync::Mutex<W>>,
    mut rx: mpsc::Receiver<Vec<u8>>,
) -> JoinHandle<()>
where
    W: Write + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            let mut guard = writer.lock().await;
            let _ = guard.write_all(&bytes);
            let _ = guard.flush();
        }
    })
}
