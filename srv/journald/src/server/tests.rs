use std::sync::Arc;
use std::time::Duration;

use super::JournalServerConfig;
use super::run_with_watchdog_tick;
use crate::JournalRpcClient;
use tokio::sync::Notify;
use tokio::time::timeout;

#[tokio::test]
async fn displaced_server_exits_without_unlinking_its_replacement() {
    let home = tempfile::tempdir().expect("temporary directory");
    let config = JournalServerConfig::from_chaos_home(home.path());
    let socket = config.socket_path.clone();
    let watching = Arc::new(Notify::new());
    let tick = Arc::new(Notify::new());
    let server = tokio::spawn(run_with_watchdog_tick(config, {
        let watching = watching.clone();
        let tick = tick.clone();
        move || {
            watching.notify_one();
            tick.clone().notified_owned()
        }
    }));

    let client = JournalRpcClient::new(socket.clone());
    timeout(Duration::from_secs(5), watching.notified())
        .await
        .expect("journald bound its socket and armed its watchdog");
    tick.notify_one();
    timeout(Duration::from_secs(5), watching.notified())
        .await
        .expect("watchdog keeps watching the original socket");
    timeout(Duration::from_secs(5), client.hello("test"))
        .await
        .expect("journald answers before timeout")
        .expect("journald hello");

    std::fs::remove_file(&socket).expect("unlink journald socket");
    let _replacement = std::os::unix::net::UnixListener::bind(&socket).expect("bind replacement");
    tick.notify_one();

    timeout(Duration::from_secs(10), server)
        .await
        .expect("displaced journald exits")
        .expect("join journald")
        .expect("journald shuts down cleanly");
    assert!(socket.exists(), "replacement socket must survive");
}
