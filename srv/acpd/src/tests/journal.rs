//! Own a matching journal for this test executable, never the installed daemon.

use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use chaos_journald::{JournalRpcClient, JournalServerConfig, run_sqlite_journal_server};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

static JOURNAL: Mutex<Option<Journal>> = Mutex::new(None);

struct Journal {
    stop: CancellationToken,
    thread: Option<JoinHandle<()>>,
    home: TempDir,
}

#[ctor::ctor(unsafe)]
fn isolate() {
    // Short paths also fit macOS's Unix-domain socket path limit.
    let home = TempDir::new_in("/tmp").expect("ACP journal fixture directory");
    // SAFETY: this constructor runs before test/runtime threads are started.
    unsafe { std::env::set_var("CHAOS_JOURNALD_SOCKET", home.path().join("j.sock")) };
    *JOURNAL.lock().unwrap() = Some(Journal {
        stop: CancellationToken::new(),
        thread: None,
        home,
    });
    // SAFETY: the callback has C ABI and only joins this fixture's own thread.
    assert_eq!(unsafe { libc::atexit(close) }, 0);
}

pub(super) fn start() {
    let mut journal = JOURNAL.lock().unwrap();
    let journal = journal.as_mut().expect("isolated ACP journal configured");
    if journal.thread.is_some() {
        return;
    }
    let config = JournalServerConfig {
        socket_path: journal.home.path().join("j.sock"),
        sqlite_db_path: journal.home.path().join("journal.sqlite"),
    };
    let cancelled = journal.stop.clone();
    let (ready, started) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("ACP journal fixture runtime");
        runtime.block_on(async move {
            let client = JournalRpcClient::new(config.socket_path.clone());
            let server = tokio::spawn(run_sqlite_journal_server(config));
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if client.hello("acp-native-tests").await.is_ok() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("ACP journal fixture startup");
            ready.send(()).expect("ACP journal fixture owner");
            cancelled.cancelled().await;
            server.abort();
            let _ = server.await;
        });
    });
    started
        .recv_timeout(Duration::from_secs(6))
        .expect("ACP journal fixture ready");
    journal.thread = Some(thread);
}

extern "C" fn close() {
    if let Some(journal) = JOURNAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        journal.stop.cancel();
        if let Some(thread) = journal.thread {
            let _ = thread.join();
        }
    }
}
